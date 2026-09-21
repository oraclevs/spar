use crate::ast::*;
use crate::error::{Span, SparError};
use crate::shell_lang::{
    parse_bare_command_statement, parse_command_expression, parse_command_substitution,
};
use crate::token::{SpannedToken, Token};

type RunHeader = (RunShell, Option<Span>, Option<String>, Option<Span>);

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
    active_type_parameters: Vec<TypeParameter>,
    /// Interactive sessions accept a bare expression as a module-level
    /// statement, so `answer + 1` or `users |> take(2)` can be previewed.
    interactive: bool,
}

fn parse_error_start(error: &SparError) -> usize {
    match error {
        SparError::ParseError { span, .. } | SparError::LexError { span, .. } => span.start,
        _ => 0,
    }
}

fn statement_span(statement: &Statement) -> Span {
    match statement {
        Statement::LocalVar(declaration) => declaration.span.clone(),
        Statement::Assignment { span, .. } | Statement::FieldAssignment { span, .. } => {
            span.clone()
        }
        Statement::Expression(_, span) | Statement::Return(_, span) => span.clone(),
        Statement::Break(span) | Statement::Continue(span) => span.clone(),
        Statement::If(statement) => statement.span.clone(),
        Statement::For(statement) => statement.span.clone(),
        Statement::Try(statement) => statement.span.clone(),
    }
}

fn mark_type_parameters(ty: SparType, parameters: &[TypeParameter]) -> SparType {
    match ty {
        SparType::Named(name) if parameters.iter().any(|parameter| parameter.name == name) => {
            SparType::TypeParameter(name)
        }
        SparType::List(inner) => SparType::List(Box::new(mark_type_parameters(*inner, parameters))),
        SparType::Applied { name, arguments } => SparType::Applied {
            name,
            arguments: arguments
                .into_iter()
                .map(|argument| mark_type_parameters(argument, parameters))
                .collect(),
        },
        other => other,
    }
}

/// Parses exactly one ordinary Spar expression from an already-tokenized
/// fragment. Native-shell mixed pipelines use this to turn the raw text after
/// `|>` back into the same expression AST used everywhere else.
pub(crate) fn parse_expression_tokens(tokens: Vec<SpannedToken>) -> Result<Expr, SparError> {
    let mut parser = Parser::new(tokens);
    let expression = parser.parse_expr()?;
    if !parser.at(&Token::Eof) {
        return Err(parser.error(format!(
            "unexpected {} after structured pipeline stage",
            parser.peek().human_name()
        )));
    }
    Ok(expression)
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self {
            tokens,
            pos: 0,
            active_type_parameters: Vec::new(),
            interactive: false,
        }
    }

    /// Accept bare expression statements at module level (interactive only).
    pub(crate) fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }

    fn peek(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .map(|st| &st.token)
            .unwrap_or(&Token::Eof)
    }

    /// Source line of the most recently consumed token.
    fn prev_line(&self) -> u32 {
        self.pos
            .checked_sub(1)
            .and_then(|index| self.tokens.get(index))
            .map(|st| st.span.line)
            .unwrap_or(0)
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
        let name = match self.peek() {
            Token::Ident(name) => Some(name.clone()),
            // Native-shell words are contextual keywords: outside their
            // construct positions they remain legal Spar names.
            Token::KwCommand => Some("command".to_string()),
            Token::KwExec => Some("exec".to_string()),
            Token::TypeShell => Some("shell".to_string()),
            _ => None,
        };
        if let Some(name) = name {
            let span = self.advance().span.clone();
            Ok((name, span))
        } else {
            Err(SparError::ParseError {
                message: format!("expected a name, found {}", self.peek().human_name()),
                span: self.peek_span(),
            })
        }
    }

    fn at(&self, tok: &Token) -> bool {
        self.peek() == tok
    }

    fn next_is(&self, tok: &Token) -> bool {
        self.tokens
            .get(self.pos + 1)
            .map(|spanned| &spanned.token == tok)
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    fn at_ident(&self) -> bool {
        matches!(
            self.peek(),
            Token::Ident(_) | Token::KwCommand | Token::KwExec | Token::TypeShell
        )
    }

    fn error(&self, msg: impl Into<String>) -> SparError {
        SparError::ParseError {
            message: msg.into(),
            span: self.peek_span(),
        }
    }

    pub fn parse(mut self) -> Result<Program, SparError> {
        let load_env = if self.at(&Token::At) {
            self.advance(); // consume '@'
            let (name, name_span) = self.expect_ident()?;
            match name.as_str() {
                "SchemaFile" => {
                    return Err(SparError::ParseError {
                        message: "@SchemaFile was removed; declare `schema Name { ... };` \
                                  (any file containing `schema` declarations is a schema file)"
                            .to_string(),
                        span: name_span,
                    });
                }
                "LoadEnv" => {
                    let path = if self.at(&Token::LParen) {
                        self.advance();
                        let path = self.parse_load_env_path()?;
                        self.expect(&Token::RParen)?;
                        path
                    } else {
                        ".env".to_string()
                    };
                    Some(path)
                }
                _ => {
                    return Err(SparError::ParseError {
                        message: format!(
                            "unknown file pragma `@{name}`; only `@LoadEnv` is supported"
                        ),
                        span: name_span,
                    });
                }
            }
        } else {
            None
        };

        let mut items = Vec::new();
        loop {
            if self.at(&Token::Eof) {
                break;
            }
            items.push(self.parse_top_level_item_or_expression()?);
        }

        let is_schema_file = items.iter().any(|item| {
            matches!(
                item,
                TopLevelItem::SchemaSection(_) | TopLevelItem::SchemaFrom(_)
            )
        });

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
                        TopLevelItem::Impl(d) => d.span.clone(),
                        TopLevelItem::SchemaFrom(d) => d.span.clone(),
                        TopLevelItem::Task(d) => d.span.clone(),
                        TopLevelItem::Statement(s) => statement_span(s),
                        TopLevelItem::SchemaSection(_) => unreachable!(),
                    };
                    return Err(SparError::ParseError {
                        message:
                            "schema files may only contain `schema Name {...};` declarations, \
                                   `schema Name from Type;`, and `import type {...} from \"...\";`"
                                .to_string(),
                        span: item_span,
                    });
                }
            }
        }

        Ok(Program {
            is_schema_file,
            load_env,
            items,
            shebang: None,
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

    fn parse_top_level_item_or_expression(&mut self) -> Result<TopLevelItem, SparError> {
        if !self.interactive {
            return self.parse_top_level_item();
        }
        let start = self.pos;
        let original = match self.parse_top_level_item() {
            Ok(item) => return Ok(item),
            Err(error) => error,
        };
        self.pos = start;
        let span = self.peek_span();
        let expression_error = match self.parse_expr() {
            Ok(expression) if self.at(&Token::Semicolon) => {
                self.advance();
                return Ok(TopLevelItem::Statement(Statement::Expression(
                    expression, span,
                )));
            }
            Ok(expression) if self.at(&Token::Eof) => {
                return Ok(TopLevelItem::Statement(Statement::Expression(
                    expression, span,
                )));
            }
            Ok(_) => self.error(format!(
                "unexpected {} after interactive expression",
                self.peek().human_name()
            )),
            Err(error) => error,
        };
        self.pos = start;
        if parse_error_start(&expression_error) > parse_error_start(&original) {
            Err(expression_error)
        } else {
            Err(original)
        }
    }

    fn parse_top_level_item(&mut self) -> Result<TopLevelItem, SparError> {
        if !self.at(&Token::HashBracket) {
            return self.parse_unattributed_top_level_item();
        }
        let attributes = self.parse_attributes()?;
        let placement_error = |attribute: &Attribute| SparError::ParseError {
            message: format!(
                "attribute `#[{}]` is only valid on top-level structs and vars",
                attribute.name
            ),
            span: attribute.span.clone(),
        };
        if self.at(&Token::Eof) {
            return Err(placement_error(&attributes[0]));
        }
        let mut item = self.parse_unattributed_top_level_item()?;
        match &mut item {
            TopLevelItem::Var(declaration) => declaration.attributes = attributes,
            TopLevelItem::Section(declaration) => declaration.attributes = attributes,
            _ => return Err(placement_error(&attributes[0])),
        }
        Ok(item)
    }

    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, SparError> {
        let mut attributes = Vec::new();
        while self.at(&Token::HashBracket) {
            let span = self.peek_span();
            self.advance();
            let (name, name_span) = self.expect_ident()?;
            if !KNOWN_ATTRIBUTES.contains(&name.as_str()) {
                return Err(SparError::ParseError {
                    message: format!(
                        "unknown attribute `#[{name}]`; valid attributes: {}",
                        KNOWN_ATTRIBUTES.join(", ")
                    ),
                    span: name_span,
                });
            }
            self.expect(&Token::RBracket)?;
            attributes.push(Attribute { name, span });
        }
        Ok(attributes)
    }

    fn parse_unattributed_top_level_item(&mut self) -> Result<TopLevelItem, SparError> {
        match self.peek() {
            Token::Import     => Ok(TopLevelItem::Import(self.parse_import()?)),
            Token::Var        => Ok(TopLevelItem::Var(self.parse_var_decl(false)?)),
            Token::Dynamic    => Ok(TopLevelItem::Dynamic(self.parse_dynamic_decl()?)),
            Token::LBracket   => self.parse_section(false, false),
            Token::KwStruct   => self.parse_struct(false, false),
            Token::KwImpl     => Ok(TopLevelItem::Impl(self.parse_impl_decl()?)),
            Token::KwFunction => Ok(TopLevelItem::Function(
                self.parse_top_level_function_decl(false, false)?,
            )),
            Token::KwAsync => Ok(TopLevelItem::Function(
                self.parse_top_level_function_decl(false, true)?,
            )),
            Token::Ident(s) if s == "type" => Ok(TopLevelItem::Type(self.parse_type_decl(false)?)),
            Token::Ident(s) if s == "enum" => Ok(TopLevelItem::Enum(self.parse_enum_decl(false)?)),
            Token::Ident(s) if s == "functionGroup" => Ok(TopLevelItem::FunctionGroup(self.parse_function_group_decl(false)?)),
            Token::Ident(s) if s == "Schema" && self.next_is(&Token::LBracket) => Err(self.error(
                "Schema [Name]{...} was replaced by `schema Name { ... };`",
            )),
            Token::Ident(s) if s == "SchemaFrom" && self.next_is(&Token::LBracket) => Err(self.error(
                "SchemaFrom [Name, Type]; was replaced by `schema Name from Type;`",
            )),
            Token::Ident(s) if s == "schema" && self.schema_declaration_follows() => {
                self.parse_schema_item()
            }
            Token::Ident(s) if s == "task" => Ok(TopLevelItem::Task(Box::new(self.parse_task_decl()?))),
            Token::KwIf | Token::KwFor | Token::KwBreak | Token::KwContinue => {
                Ok(TopLevelItem::Statement(self.parse_func_stmt()?))
            }
            Token::Ident(_)
            | Token::KwCommand
            | Token::KwExec
            | Token::TypeShell
            | Token::TypeStr
            | Token::TypeInt
            | Token::TypeFloat
            | Token::TypeBool => Ok(TopLevelItem::Statement(self.parse_func_stmt()?)),
            Token::Export => {
                self.advance();
                match self.peek() {
                    Token::Var      => Ok(TopLevelItem::Var(self.parse_var_decl(true)?)),
                    Token::LBracket => self.parse_section(true, false),
                    Token::KwStruct => self.parse_struct(true, false),
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
                        Err(self.error(
                            "'private' cannot be used with variables — \
                             use 'var' for private variables (they are not emitted by default) \
                             or 'export var' to include them in output".to_string()
                        ))
                    }
                    Token::LBracket => {
                        let item = self.parse_section(false, true)?;
                        Ok(item)
                    }
                    Token::KwStruct => self.parse_struct(false, true),
                    Token::KwFunction => {
                        Ok(TopLevelItem::Function(
                            self.parse_top_level_function_decl(true, false)?,
                        ))
                    }
                    Token::KwAsync => Ok(TopLevelItem::Function(
                        self.parse_top_level_function_decl(true, true)?,
                    )),
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
                "'@LoadEnv' pragma must be the first item in the file; \
                 pragmas cannot appear mid-file"
            )),
            _ => Err(self.error(format!(
                "unexpected {}: expected 'import', 'var', 'export', 'dynamic', 'private', 'function', 'functionGroup', 'type', 'schema', 'task', or '[' to start a declaration",
                self.peek().human_name()
            ))),
        }
    }

    fn parse_import(&mut self) -> Result<ImportDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::Import)?;
        let package = if matches!(self.peek(), Token::Ident(name) if name == "pkg") {
            self.advance();
            true
        } else {
            false
        };

        // `import schema "path";`
        if matches!(self.peek(), Token::Ident(s) if s == "schema") {
            if package {
                return Err(self.error(
                    "`import pkg schema` is not supported; schema imports are local modules",
                ));
            }
            self.advance();
            let path = self.parse_import_path()?;
            self.expect(&Token::Semicolon)?;
            return Ok(ImportDecl {
                path,
                package,
                kind: ImportKind::Schema,
                span,
            });
        }

        if matches!(self.peek(), Token::Ident(s) if s == "asPartOf") {
            return Err(self.error(
                "`asPartOf` imports were removed; use `import { Name } from \"./module\";` \
                 for selective imports or `import \"./module\" as alias;` for a module namespace",
            ));
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
                package,
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
                package,
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
            package,
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

        let mutable = if self.at(&Token::KwMut) {
            self.advance();
            true
        } else {
            false
        };

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
            mutable,
            name,
            optional,
            ty,
            value,
            span,
            attributes: Vec::new(),
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
        if self.at(&Token::HashBracket) {
            return Err(
                self.error("attribute `#[...]` is only valid on top-level structs and vars")
            );
        }
        if self.at(&Token::DotDotDot) {
            Ok(SectionItem::Spread(self.parse_spread()?))
        } else if self.at_ident() {
            Ok(SectionItem::Field(self.parse_field_decl()?))
        } else {
            Err(self.error("expected a field declaration or `...` spread"))
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
            | Token::TypeSection
            | Token::TypeShell => true,
            // A bare Ident is only a type-start when immediately followed by
            // `=` — otherwise it's the type-omitted value form (`name: someVar;`).
            Token::Ident(_) | Token::KwCommand | Token::KwExec => {
                if matches!(
                    self.tokens.get(self.pos + 1).map(|st| &st.token),
                    Some(Token::Eq)
                ) {
                    true
                } else if matches!(
                    self.tokens.get(self.pos + 1).map(|st| &st.token),
                    Some(Token::Lt)
                ) {
                    let mut depth = 0usize;
                    let mut index = self.pos + 1;
                    loop {
                        match self.tokens.get(index).map(|token| &token.token) {
                            Some(Token::Lt) => depth += 1,
                            Some(Token::Gt) => {
                                depth -= 1;
                                if depth == 0 {
                                    break matches!(
                                        self.tokens.get(index + 1).map(|token| &token.token),
                                        Some(Token::Eq) | Some(Token::Semicolon)
                                    );
                                }
                            }
                            Some(Token::Eof) | None => break false,
                            _ => {}
                        }
                        index += 1;
                    }
                } else {
                    false
                }
            }
            Token::LBracket => {
                matches!(
                    self.tokens.get(self.pos + 1).map(|st| &st.token),
                    Some(Token::TypeStr)
                        | Some(Token::TypeInt)
                        | Some(Token::TypeFloat)
                        | Some(Token::TypeBool)
                        | Some(Token::KwFn)
                ) || (
                    // `[Ident] =` — list of a named type, same `=`-disambiguation.
                    matches!(
                        self.tokens.get(self.pos + 1).map(|st| &st.token),
                        Some(Token::Ident(_)) | Some(Token::KwCommand) | Some(Token::KwExec)
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

        // Canonical structs use `field = value;` when a target type supplies
        // field types. Legacy sections retain the colon-based forms below.
        if self.at(&Token::Eq) {
            self.advance();
            let value = if self.at(&Token::LBrace) {
                self.expect(&Token::LBrace)?;
                let mut items = Vec::new();
                while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                    items.push(self.parse_section_item()?);
                }
                self.expect(&Token::RBrace)?;
                FieldValue::Nested(items)
            } else {
                FieldValue::Expr(self.parse_expr()?)
            };
            self.expect(&Token::Semicolon)?;
            return Ok(FieldDecl {
                name,
                optional,
                ty: None,
                value: Some(value),
                span,
                end_line: self.prev_line(),
            });
        }

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
                end_line: self.prev_line(),
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
                end_line: self.prev_line(),
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

    /// `schema Name ...` / `schema? Name ...` — a declaration, not a
    /// variable or call that happens to be named `schema`.
    fn schema_declaration_follows(&self) -> bool {
        matches!(
            self.tokens.get(self.pos + 1).map(|t| &t.token),
            Some(Token::Question) | Some(Token::Ident(_))
        )
    }

    /// `schema [?] Name { fields };` or `schema [?] Name from Type;`.
    fn parse_schema_item(&mut self) -> Result<TopLevelItem, SparError> {
        let span = self.peek_span();
        self.advance(); // consume 'schema'
        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };
        let (name, _) = self.expect_ident()?;
        if matches!(self.peek(), Token::Ident(word) if word == "from") {
            self.advance(); // consume 'from'
            let (source_type, source_type_span) = self.expect_ident()?;
            self.expect(&Token::Semicolon)?;
            return Ok(TopLevelItem::SchemaFrom(SchemaFromDecl {
                name,
                source_type,
                source_type_span,
                marker: SchemaMarker { optional },
                span,
            }));
        }
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            fields.push(self.parse_schema_field()?);
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;
        Ok(TopLevelItem::SchemaSection(SchemaSectionDecl {
            name,
            marker: SchemaMarker { optional },
            fields,
            span,
        }))
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
        let mut variant_lines = Vec::new();
        if !self.at(&Token::RBrace) {
            let (v, variant_span) = self.expect_ident()?;
            variants.push(v);
            variant_lines.push(variant_span.line);
            while self.at(&Token::Comma) {
                self.advance();
                if self.at(&Token::RBrace) {
                    break; // trailing comma
                }
                let (v, variant_span) = self.expect_ident()?;
                variants.push(v);
                variant_lines.push(variant_span.line);
            }
        }
        self.expect(&Token::RBrace)?;
        let end_line = self.prev_line();
        self.expect(&Token::Semicolon)?;
        Ok(EnumDecl {
            name,
            name_span,
            exported,
            variants,
            span,
            variant_lines,
            end_line,
        })
    }

    fn parse_type_decl(&mut self, exported: bool) -> Result<TypeDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume the 'type' ident
        let legacy = self.at(&Token::LBracket);
        if legacy {
            self.advance();
        }
        let (name, name_span) = self.expect_ident()?;
        let type_parameters = self.parse_type_parameters()?;
        if legacy {
            self.expect(&Token::RBracket)?;
        }
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            fields.push(self.parse_type_field(&type_parameters)?);
        }
        self.expect(&Token::RBrace)?;
        let end_line = self.prev_line();
        self.expect(&Token::Semicolon)?;
        Ok(TypeDecl {
            name,
            name_span,
            type_parameters,
            exported,
            fields,
            span,
            end_line,
        })
    }

    /// Parse a single type field: `name: Type;`, `name?: Type;`,
    /// `name: OtherDeclaredType;`, or `name: section = { ... };`.
    fn parse_type_field(
        &mut self,
        type_parameters: &[TypeParameter],
    ) -> Result<TypeField, SparError> {
        let span = self.peek_span();
        let (name, _) = self.expect_ident()?;

        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };

        self.expect(&Token::Colon)?;
        let shape = self.parse_type_field_shape(type_parameters)?;
        let default = if self.at(&Token::Eq) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&Token::Semicolon)?;
        Ok(TypeField {
            name,
            optional,
            shape,
            default,
            span,
        })
    }

    fn parse_type_field_shape(
        &mut self,
        type_parameters: &[TypeParameter],
    ) -> Result<TypeFieldShape, SparError> {
        if self.at(&Token::TypeSection) {
            self.advance(); // consume 'section'
            self.expect(&Token::Eq)?;
            self.expect(&Token::LBrace)?;
            let mut nested = Vec::new();
            while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                nested.push(self.parse_type_field(type_parameters)?);
            }
            self.expect(&Token::RBrace)?;
            Ok(TypeFieldShape::Section(nested))
        } else {
            let ty = mark_type_parameters(self.parse_type()?, type_parameters);
            Ok(match ty {
                SparType::Named(name) => TypeFieldShape::Named(name),
                SparType::TypeParameter(name) => TypeFieldShape::TypeParameter(name),
                SparType::Applied { name, arguments } => {
                    TypeFieldShape::Applied { name, arguments }
                }
                primitive => TypeFieldShape::Primitive(primitive),
            })
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

        let (items, end_line) = self.parse_regular_section_items()?;
        Ok(TopLevelItem::Section(SectionDecl {
            exported,
            private,
            canonical: false,
            path: vec![name],
            items,
            type_binding,
            span,
            end_line,
            attributes: Vec::new(),
        }))
    }

    fn parse_impl_decl(&mut self) -> Result<ImplDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwImpl)?;
        let type_parameters = self.parse_type_parameters()?;
        let previous_type_parameters =
            std::mem::replace(&mut self.active_type_parameters, type_parameters.clone());
        let target = mark_type_parameters(self.parse_type()?, &type_parameters);
        self.expect(&Token::LBrace)?;
        let mut methods = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            let is_private = if self.at(&Token::Private) {
                self.advance();
                true
            } else {
                false
            };
            methods.push(self.parse_impl_method(target.clone(), is_private)?);
            self.expect(&Token::Semicolon)?;
        }
        let end_line = self.peek_span().line;
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;
        self.active_type_parameters = previous_type_parameters;
        Ok(ImplDecl {
            target,
            type_parameters,
            methods,
            span,
            end_line,
        })
    }

    fn parse_impl_method(
        &mut self,
        target: SparType,
        is_private: bool,
    ) -> Result<ImplMethodDecl, SparError> {
        let span = self.peek_span();
        let is_async = if self.at(&Token::KwAsync) {
            self.advance();
            true
        } else {
            false
        };
        self.expect(&Token::KwFunction)?;
        let (name, name_span) = self.expect_ident()?;
        let type_parameters = self.parse_type_parameters()?;
        let previous_type_parameters =
            std::mem::replace(&mut self.active_type_parameters, type_parameters.clone());
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        let mut receiver = None;
        let mut saw_default = false;
        let mut first = true;
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            let param_span = self.peek_span();
            if first && self.at(&Token::KwMut) {
                self.advance();
                let (name, _) = self.expect_ident()?;
                if name != "self" {
                    return Err(self.error("`mut` in a method receiver must be followed by `self`"));
                }
                receiver = Some(MethodReceiver {
                    mutable: true,
                    span: param_span.clone(),
                });
                params.push(Param {
                    name,
                    ty: target.clone(),
                    default: None,
                    span: param_span,
                });
            } else if first && matches!(self.peek(), Token::Ident(name) if name == "self") {
                let (name, _) = self.expect_ident()?;
                receiver = Some(MethodReceiver {
                    mutable: false,
                    span: param_span.clone(),
                });
                params.push(Param {
                    name,
                    ty: target.clone(),
                    default: None,
                    span: param_span,
                });
            } else {
                let (param_name, _) = self.expect_ident()?;
                if param_name == "self" {
                    return Err(self.error("`self` must be the first method parameter"));
                }
                self.expect(&Token::Colon)?;
                let ty = mark_type_parameters(self.parse_type()?, &type_parameters);
                let default = if self.at(&Token::Eq) {
                    self.advance();
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if default.is_none() && saw_default {
                    return Err(self.error(
                        "a required function parameter cannot follow a parameter with a default",
                    ));
                }
                saw_default |= default.is_some();
                params.push(Param {
                    name: param_name,
                    ty,
                    default,
                    span: param_span,
                });
            }
            first = false;
            if self.at(&Token::Comma) {
                self.advance();
            }
        }
        self.expect(&Token::RParen)?;
        self.expect(&Token::Arrow)?;
        let ret_span = self.peek_span();
        let ret = mark_type_parameters(self.parse_function_return_type()?, &type_parameters);
        self.expect(&Token::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            stmts.push(self.parse_func_stmt()?);
        }
        let body_span = self.peek_span();
        self.expect(&Token::RBrace)?;
        self.active_type_parameters = previous_type_parameters;
        Ok(ImplMethodDecl {
            function: FunctionDecl {
                name,
                name_span,
                type_parameters,
                params,
                ret,
                ret_span,
                body: FunctionBody {
                    stmts,
                    span: body_span,
                },
                is_async,
                is_private,
                trusted_native: false,
                span,
            },
            receiver,
        })
    }

    fn parse_struct(&mut self, exported: bool, private: bool) -> Result<TopLevelItem, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwStruct)?;
        let (name, _) = self.expect_ident()?;
        if self.at(&Token::Lt) {
            return Err(self.error(
                "structs are concrete values and cannot declare type parameters; \
                 declare a generic `type` and instantiate it from the struct",
            ));
        }
        let type_binding = if self.at(&Token::Colon) {
            self.advance();
            let type_span = self.peek_span();
            Some(TypeBinding {
                ty: self.parse_type()?,
                span: type_span,
            })
        } else {
            None
        };
        let (items, end_line) = self.parse_regular_section_items()?;
        Ok(TopLevelItem::Section(SectionDecl {
            exported,
            private,
            canonical: true,
            path: vec![name],
            items,
            type_binding,
            span,
            end_line,
            attributes: Vec::new(),
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
        let name_span = self.peek_span();
        let ty = self.parse_type()?;
        Ok(Some(TypeBinding {
            ty,
            span: name_span,
        }))
    }

    /// Parse a regular section body: `{ ...fields/spreads... };`, including
    /// the trailing `;`.
    fn parse_regular_section_items(&mut self) -> Result<(Vec<SectionItem>, u32), SparError> {
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
        let end_line = self.prev_line();
        self.expect(&Token::Semicolon)?;
        Ok((items, end_line))
    }

    /// A function's `-> ...` type. Identical to `parse_type` except it also
    /// accepts `void`, which is only legal here — no other type position
    /// recognizes the `void` keyword, so a var/param/field/list-element
    /// type of `void` is rejected by `parse_type`'s ordinary "expected a
    /// type" error rather than needing a dedicated semantic check.
    fn parse_function_return_type(&mut self) -> Result<SparType, SparError> {
        if self.at(&Token::TypeVoid) {
            self.advance();
            return Ok(SparType::Void);
        }
        self.parse_type()
    }

    fn parse_type_parameters(&mut self) -> Result<Vec<TypeParameter>, SparError> {
        if !self.at(&Token::Lt) {
            return Ok(Vec::new());
        }
        self.advance();
        if self.at(&Token::Gt) {
            return Err(self.error("generic parameter list cannot be empty"));
        }
        let mut parameters = Vec::new();
        loop {
            let span = self.peek_span();
            let (name, _) = self.expect_ident()?;
            parameters.push(TypeParameter { name, span });
            if !self.at(&Token::Comma) {
                break;
            }
            self.advance();
            if self.at(&Token::Gt) {
                break;
            }
        }
        self.expect(&Token::Gt)?;
        Ok(parameters)
    }

    fn parse_type_arguments_required(&mut self) -> Result<Vec<SparType>, SparError> {
        self.expect(&Token::Lt)?;
        if self.at(&Token::Gt) {
            return Err(self.error("generic argument list cannot be empty"));
        }
        let mut arguments = Vec::new();
        loop {
            let argument = self.parse_type()?;
            arguments.push(mark_type_parameters(argument, &self.active_type_parameters));
            if !self.at(&Token::Comma) {
                break;
            }
            self.advance();
            if self.at(&Token::Gt) {
                break;
            }
        }
        self.expect(&Token::Gt)?;
        Ok(arguments)
    }

    fn try_parse_call_type_arguments(&mut self) -> Result<Option<Vec<SparType>>, SparError> {
        if !self.at(&Token::Lt) {
            return Ok(None);
        }
        if self.tokens.get(self.pos + 1).map(|token| &token.token) == Some(&Token::Gt) {
            return Err(self.error("generic argument list cannot be empty"));
        }
        let checkpoint = self.pos;
        match self.parse_type_arguments_required() {
            Ok(arguments) if self.at(&Token::LParen) => Ok(Some(arguments)),
            Ok(_) | Err(_) => {
                self.pos = checkpoint;
                Ok(None)
            }
        }
    }

    fn parse_type(&mut self) -> Result<SparType, SparError> {
        if self.at(&Token::KwFn) {
            return self.parse_callable_type();
        }
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

    fn parse_callable_type(&mut self) -> Result<SparType, SparError> {
        self.expect(&Token::KwFn)?;
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            params.push(self.parse_type()?);
            if self.at(&Token::Comma) {
                self.advance();
                if self.at(&Token::RParen) {
                    break;
                }
            } else if !self.at(&Token::RParen) {
                return Err(self.error(format!(
                    "expected ',' or ')', found {}",
                    self.peek().human_name()
                )));
            }
        }
        self.expect(&Token::RParen)?;
        self.expect(&Token::Arrow)?;
        let return_type = self.parse_function_return_type()?;
        Ok(SparType::Function {
            params,
            return_type: Box::new(return_type),
        })
    }

    fn parse_scalar_type(&mut self) -> Result<SparType, SparError> {
        if matches!(
            self.peek(),
            Token::Ident(_) | Token::KwCommand | Token::KwExec
        ) {
            // Native-shell words are contextual here too: a user-declared
            // type named `command` or `exec` remains referenceable outside
            // the actual native-shell construct positions.
            let (name, _) = self.expect_ident()?;
            if self.at(&Token::Lt) {
                let arguments = self.parse_type_arguments_required()?;
                if name == "List" {
                    let [inner] = arguments.as_slice() else {
                        return Err(self.error("List expects exactly one type argument"));
                    };
                    return Ok(SparType::List(Box::new(inner.clone())));
                }
                return Ok(SparType::Applied { name, arguments });
            }
            if self
                .active_type_parameters
                .iter()
                .any(|parameter| parameter.name == name)
            {
                return Ok(SparType::TypeParameter(name));
            }
            return Ok(SparType::Named(name));
        }
        let ty = match self.peek() {
            Token::TypeStr     => SparType::Str,
            Token::TypeInt     => SparType::Int,
            Token::TypeFloat   => SparType::Float,
            Token::TypeBool    => SparType::Bool,
            Token::TypeSection => SparType::Section,
            Token::TypeShell   => SparType::Shell,
            _ => return Err(self.error(format!("expected a type ('str', 'int', 'float', 'bool', 'section', 'shell', or a declared type name), found {}", self.peek().human_name()))),
        };
        self.advance();
        Ok(ty)
    }

    fn parse_expr(&mut self) -> Result<Expr, SparError> {
        self.parse_structured_pipe_expr()
    }

    fn parse_structured_pipe_expr(&mut self) -> Result<Expr, SparError> {
        let mut input = self.parse_fallback_expr()?;
        while self.at(&Token::StructuredPipe) {
            let span = self.peek_span();
            self.advance();
            let stage = self.parse_fallback_expr()?;
            input = Expr::StructuredPipe {
                input: Box::new(input),
                stage: Box::new(stage),
                span,
            };
        }
        Ok(input)
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
        if self.at(&Token::KwAwait) {
            let span = self.peek_span();
            self.advance();
            let value = self.parse_unary()?;
            return Ok(Expr::Await {
                value: Box::new(value),
                span,
            });
        }
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
            Token::KwFn => self.parse_closure_expr(),
            Token::LBracket => self.parse_list_literal(),
            Token::LBrace => self.parse_object_literal(),
            Token::LParen => {
                let span = self.peek_span();
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(Expr::Grouped(Box::new(inner), span))
            }
            Token::Ident(_) | Token::TypeShell => self.parse_namespace_ref_or_fn_call(),
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
            Token::ShellBlockStart | Token::ShellForeignBlockStart(_) => {
                self.parse_mixed_shell_block()
            }
            Token::KwCommand
                if matches!(
                    self.tokens.get(self.pos + 1).map(|token| &token.token),
                    Some(Token::ShellWord(_)) | Some(Token::ShellLiteralWord(_))
                ) =>
            {
                let (shell, consumed) = parse_command_expression(&self.tokens[self.pos..])?;
                // The command expression's terminating semicolon is also the
                // containing Spar statement's semicolon, so leave it for the
                // caller's ordinary statement parser to consume.
                self.pos += consumed - 1;
                Ok(Expr::Shell(shell))
            }
            Token::KwExec if self.next_is(&Token::ShellBlockStart) => {
                let exec_span = self.peek_span();
                self.advance();
                let Expr::Shell(shell) = self.parse_mixed_shell_block()? else {
                    unreachable!("mixed shell parser always returns Expr::Shell")
                };
                if !shell.statements.is_empty() || shell.steps.len() != 1 {
                    return Err(SparError::ParseError {
                        message: "'exec { ... }' must contain exactly one command or pipeline"
                            .into(),
                        span: exec_span,
                    });
                }
                Ok(Expr::ExecShell(shell))
            }
            Token::KwCommand | Token::KwExec => self.parse_namespace_ref_or_fn_call(),
            Token::CommandSubStart => {
                let (shell, consumed) = parse_command_substitution(&self.tokens[self.pos..])?;
                self.pos += consumed;
                Ok(Expr::CommandSubstitution(shell))
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
                if self.at(&Token::LParen) {
                    self.advance();
                    let mut args = Vec::new();
                    while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
                        args.push(self.parse_expr()?);
                        if self.at(&Token::Comma) {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.expect(&Token::RParen)?;
                    expr = Expr::MethodCall {
                        receiver: Box::new(expr),
                        method: field,
                        method_span: field_span,
                        args,
                        span,
                    };
                } else {
                    expr = Expr::FieldAccess {
                        base: Box::new(expr),
                        field,
                        field_span,
                        span,
                    };
                }
            }
        }

        Ok(expr)
    }

    fn parse_closure_expr(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwFn)?;
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            let param_span = self.peek_span();
            let (name, _) = self.expect_ident()?;
            let ty = if self.at(&Token::Colon) {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            params.push(ClosureParam {
                name,
                ty,
                span: param_span,
            });
            if self.at(&Token::Comma) {
                self.advance();
                if self.at(&Token::RParen) {
                    break;
                }
            } else if !self.at(&Token::RParen) {
                return Err(self.error(format!(
                    "expected ',' or ')' in closure parameter list, found {}",
                    self.peek().human_name()
                )));
            }
        }
        self.expect(&Token::RParen)?;

        let return_type = if self.at(&Token::Arrow) {
            self.advance();
            Some(self.parse_function_return_type()?)
        } else {
            None
        };

        let body = if self.at(&Token::FatArrow) {
            self.advance();
            ClosureBody::Expr(Box::new(self.parse_fallback_expr()?))
        } else if self.at(&Token::LBrace) {
            self.advance();
            let mut stmts = Vec::new();
            while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                stmts.push(self.parse_func_stmt()?);
            }
            let body_span = self.peek_span();
            self.expect(&Token::RBrace)?;
            ClosureBody::Block(FunctionBody {
                stmts,
                span: body_span,
            })
        } else {
            return Err(self.error(format!(
                "expected '=>' or '{{' to start closure body, found {}",
                self.peek().human_name()
            )));
        };

        Ok(Expr::Closure {
            params,
            return_type,
            body,
            span,
        })
    }

    fn parse_mixed_shell_block(&mut self) -> Result<Expr, SparError> {
        let start_token = self.tokens[self.pos].clone();
        let foreign_shell = match &start_token.token {
            Token::ShellBlockStart => None,
            Token::ShellForeignBlockStart(shell) => Some(shell.clone()),
            _ => return Err(self.error("expected a shell block")),
        };
        self.advance();
        let start = start_token.span;
        let mut statements = Vec::new();
        while !self.at(&Token::ShellBlockEnd) && !self.at(&Token::Eof) {
            statements.push(self.parse_func_stmt()?);
        }
        let end = self.expect(&Token::ShellBlockEnd)?.span;
        let all_commands = statements.iter().all(|statement| {
            matches!(statement, Statement::Expression(Expr::Shell(shell), _)
                if shell.statements.is_empty()
                    && !shell.steps.iter().any(|(_, step)| match step {
                        ShellStep::Command(command) => command.background,
                        ShellStep::Pipeline(commands) => commands.last().is_some_and(|command| command.background),
                        ShellStep::MixedPipeline(_) => false,
                    }))
        });
        let steps = if all_commands {
            let mut flattened = Vec::new();
            for statement in &statements {
                let Statement::Expression(Expr::Shell(shell), _) = statement else {
                    unreachable!("all_commands checked")
                };
                for (join, step) in shell.steps.iter().cloned() {
                    flattened.push((join, step));
                }
            }
            flattened
        } else {
            Vec::new()
        };
        if all_commands {
            statements.clear();
        }
        Ok(Expr::Shell(ShellExpr {
            statements,
            steps,
            span: Span::new(start.start, end.end, start.line, start.col),
            foreign_shell,
            end_line: end.line,
        }))
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
        let open = self.peek_span();
        self.expect(&Token::LBrace)?;
        let mut items = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            items.push(self.parse_object_item()?);
        }
        let close = self.expect(&Token::RBrace)?.span.clone();
        // Cover the whole `{ ... }` so the formatter can tell which comments
        // sit inside it.
        let span = Span::new(open.start, close.end, open.line, open.col);
        Ok(Expr::Object(items, span))
    }

    /// Object literals are values, not section declarations. In particular,
    /// `shell: shell;` means a field named `shell` whose value is the
    /// contextual identifier `shell`; it must not be reinterpreted as a
    /// declaration of type `shell`. Typed section fields continue to use
    /// `parse_field_decl`.
    fn parse_object_item(&mut self) -> Result<SectionItem, SparError> {
        if self.at(&Token::DotDotDot) {
            return Ok(SectionItem::Spread(self.parse_spread()?));
        }

        let span = self.peek_span();
        let (name, _) = self.expect_ident()?;
        self.expect(&Token::Colon)?;
        let value = if self.at(&Token::LBrace) {
            self.expect(&Token::LBrace)?;
            let mut items = Vec::new();
            while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                items.push(self.parse_object_item()?);
            }
            self.expect(&Token::RBrace)?;
            FieldValue::Nested(items)
        } else {
            FieldValue::Expr(self.parse_expr()?)
        };
        self.expect(&Token::Semicolon)?;
        Ok(SectionItem::Field(FieldDecl {
            name,
            optional: false,
            ty: None,
            value: Some(value),
            span,
            end_line: self.prev_line(),
        }))
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

        if let Some(type_arguments) = self.try_parse_call_type_arguments()? {
            return self.parse_user_call(name, name_span, type_arguments);
        }

        if self.at(&Token::LParen) {
            if name == "env" || name == "str" || !self.call_uses_named_arguments() {
                return self.parse_fn_call(name, span);
            } else {
                return self.parse_user_call(name, name_span, Vec::new());
            }
        }

        let mut segments = vec![name];
        while self.at(&Token::ColonColon) {
            self.advance();
            let (seg, seg_span) = self.expect_ident()?;
            if let Some(type_arguments) = self.try_parse_call_type_arguments()? {
                let qualified = format!("{}::{}", segments.join("::"), seg);
                return self.parse_user_call(qualified, seg_span, type_arguments);
            }
            if self.at(&Token::LParen) {
                // cross-file call: alias::fn(args)
                let qualified = format!("{}::{}", segments.join("::"), seg);
                return self.parse_user_call(qualified, seg_span, Vec::new());
            }
            segments.push(seg);
        }

        Ok(Expr::NamespaceRef(NamespaceRef { segments, span }))
    }

    fn call_uses_named_arguments(&self) -> bool {
        matches!(
            (self.tokens.get(self.pos + 1), self.tokens.get(self.pos + 2)),
            (Some(first), Some(second))
                if matches!(
                    first.token,
                    Token::Ident(_) | Token::KwCommand | Token::KwExec | Token::TypeShell
                ) && second.token == Token::Colon
        )
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

    fn parse_user_call(
        &mut self,
        name: String,
        name_span: Span,
        type_arguments: Vec<SparType>,
    ) -> Result<Expr, SparError> {
        let span = name_span.clone();
        self.expect(&Token::LParen)?;
        let mut args = Vec::new();
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            let param_name_span = self.peek_span();
            let (param_name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let value = self.parse_expr()?;
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
            type_arguments,
            args,
            span,
        })
    }

    fn parse_comprehension(&mut self) -> Result<Expr, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwFor)?;
        let (var_name, var_name_span) = self.expect_ident()?;
        self.expect(&Token::KwIn)?;
        let source = self.parse_expr()?;
        self.expect(&Token::LBrace)?;
        let body = self.parse_expr()?;
        self.expect(&Token::RBrace)?;
        Ok(Expr::Comprehension {
            var_name,
            var_name_span,
            source: Box::new(source),
            body: Box::new(body),
            span,
        })
    }

    fn parse_function_decl(
        &mut self,
        is_private: bool,
        is_async: bool,
    ) -> Result<FunctionDecl, SparError> {
        let span = self.peek_span();
        if is_async {
            self.expect(&Token::KwAsync)?;
        }
        self.expect(&Token::KwFunction)?;
        let (name, name_span) = self.expect_ident()?;
        let type_parameters = self.parse_type_parameters()?;
        let previous_type_parameters =
            std::mem::replace(&mut self.active_type_parameters, type_parameters.clone());
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        let mut saw_default = false;
        while !self.at(&Token::RParen) && !self.at(&Token::Eof) {
            let param_span = self.peek_span();
            let (param_name, _) = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let ty = mark_type_parameters(self.parse_type()?, &type_parameters);
            let default = if self.at(&Token::Eq) {
                self.advance();
                Some(self.parse_expr()?)
            } else {
                None
            };
            if default.is_none() && saw_default {
                return Err(self.error(
                    "a required function parameter cannot follow a parameter with a default",
                ));
            }
            saw_default |= default.is_some();
            params.push(Param {
                name: param_name,
                ty,
                default,
                span: param_span,
            });
            if self.at(&Token::Comma) {
                self.advance();
            }
        }
        self.expect(&Token::RParen)?;
        self.expect(&Token::Arrow)?;
        let ret_span = self.peek_span();
        let ret = mark_type_parameters(self.parse_function_return_type()?, &type_parameters);
        self.expect(&Token::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            stmts.push(self.parse_func_stmt()?);
        }
        let body_span = self.peek_span();
        self.expect(&Token::RBrace)?;
        self.active_type_parameters = previous_type_parameters;
        Ok(FunctionDecl {
            name,
            name_span,
            type_parameters,
            params,
            ret,
            ret_span,
            body: FunctionBody {
                stmts,
                span: body_span,
            },
            is_async,
            is_private,
            trusted_native: false,
            span,
        })
    }

    fn parse_top_level_function_decl(
        &mut self,
        is_private: bool,
        is_async: bool,
    ) -> Result<FunctionDecl, SparError> {
        let decl = self.parse_function_decl(is_private, is_async)?;
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
        while self.at(&Token::KwFunction) || self.at(&Token::KwAsync) || self.at(&Token::Private) {
            if self.at(&Token::Private) {
                self.advance();
                let is_async = self.at(&Token::KwAsync);
                functions.push(self.parse_function_decl(true, is_async)?);
            } else {
                let is_async = self.at(&Token::KwAsync);
                functions.push(self.parse_function_decl(false, is_async)?);
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

    fn looks_like_field_assignment(&self) -> bool {
        if !matches!(self.peek(), Token::Ident(_)) {
            return false;
        }
        let mut index = self.pos + 1;
        let mut saw_field = false;
        while self
            .tokens
            .get(index)
            .is_some_and(|token| token.token == Token::Dot)
        {
            let Some(name) = self.tokens.get(index + 1) else {
                return false;
            };
            if !matches!(name.token, Token::Ident(_)) {
                return false;
            }
            saw_field = true;
            index += 2;
        }
        saw_field
            && self
                .tokens
                .get(index)
                .is_some_and(|token| token.token == Token::Eq || token.token == Token::PlusEq)
    }

    fn parse_func_stmt(&mut self) -> Result<FuncStmt, SparError> {
        if matches!(self.peek(), Token::ShellWord(_)) {
            let span = self.peek_span();
            let (shell, consumed) = parse_bare_command_statement(&self.tokens[self.pos..])?;
            self.pos += consumed;
            return Ok(FuncStmt::Expression(Expr::Shell(shell), span));
        }
        if self.at(&Token::KwTry) {
            return self.parse_try_stmt();
        }
        if self.at(&Token::KwIf) {
            return Ok(FuncStmt::If(self.parse_if_stmt()?));
        }
        if self.at(&Token::KwFor) {
            return self.parse_for_stmt();
        }
        if self.at(&Token::KwReturn) {
            let start_span = self.peek_span();
            self.advance(); // consume 'return'
            let ret_value = if self.at(&Token::Semicolon) {
                ReturnValue::Void
            } else if self.at(&Token::LBrace) {
                self.advance(); // consume '{'
                let mut fields = Vec::new();
                while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                    let field_span = self.peek_span();
                    let (field_name, _) = self.expect_ident()?;
                    self.expect(&Token::Colon)?;
                    let (ty, value) = if self.at_type_start() {
                        let ty = self.parse_type()?;
                        self.expect(&Token::Eq)?;
                        (Some(ty), self.parse_expr()?)
                    } else {
                        (None, self.parse_expr()?)
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
                ReturnValue::Expr(self.parse_expr()?)
            };
            self.expect(&Token::Semicolon)?;
            return Ok(FuncStmt::Return(ret_value, start_span));
        }
        if self.at(&Token::KwBreak) {
            let span = self.peek_span();
            self.advance();
            self.expect(&Token::Semicolon)?;
            return Ok(FuncStmt::Break(span));
        }
        if self.at(&Token::KwContinue) {
            let span = self.peek_span();
            self.advance();
            self.expect(&Token::Semicolon)?;
            return Ok(FuncStmt::Continue(span));
        }
        if self.at(&Token::Var) {
            return Ok(FuncStmt::LocalVar(self.parse_local_var_decl()?));
        }

        if self.looks_like_field_assignment() {
            let span = self.peek_span();
            let (base, _) = self.expect_ident()?;
            let mut fields = Vec::new();
            while self.at(&Token::Dot) {
                self.advance();
                let (field, _) = self.expect_ident()?;
                fields.push(field);
            }
            let compound = self.at(&Token::PlusEq);
            self.advance();
            let rhs = self.parse_expr()?;
            let value = if compound {
                let mut base_expr = Expr::NamespaceRef(NamespaceRef {
                    segments: vec![base.clone()],
                    span: span.clone(),
                });
                for field in &fields {
                    base_expr = Expr::FieldAccess {
                        base: Box::new(base_expr),
                        field: field.clone(),
                        field_span: span.clone(),
                        span: span.clone(),
                    };
                }
                Expr::BinaryOp(BinaryOp {
                    op: BinOp::Add,
                    lhs: Box::new(base_expr),
                    rhs: Box::new(rhs),
                    span: span.clone(),
                })
            } else {
                rhs
            };
            self.expect(&Token::Semicolon)?;
            return Ok(FuncStmt::FieldAssignment {
                base,
                fields,
                value,
                span,
            });
        }

        if self.at_ident() && (self.next_is(&Token::Eq) || self.next_is(&Token::PlusEq)) {
            let span = self.peek_span();
            let (name, _) = self.expect_ident()?;
            let compound = self.at(&Token::PlusEq);
            self.advance();
            let rhs = self.parse_expr()?;
            let value = if compound {
                Expr::BinaryOp(BinaryOp {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::NamespaceRef(NamespaceRef {
                        segments: vec![name.clone()],
                        span: span.clone(),
                    })),
                    rhs: Box::new(rhs),
                    span: span.clone(),
                })
            } else {
                rhs
            };
            self.expect(&Token::Semicolon)?;
            return Ok(FuncStmt::Assignment { name, value, span });
        }

        let span = self.peek_span();
        let expression = self.parse_expr()?;
        if !matches!(
            expression,
            Expr::Call { .. }
                | Expr::FnCall(_)
                | Expr::MethodCall { .. }
                | Expr::Shell(_)
                | Expr::ExecShell(_)
                | Expr::StructuredPipe { .. }
        ) {
            return Err(SparError::ParseError {
                message: "only function calls may be used as expression statements".into(),
                span,
            });
        }
        self.expect(&Token::Semicolon)?;
        Ok(FuncStmt::Expression(expression, span))
    }

    fn parse_try_stmt(&mut self) -> Result<FuncStmt, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwTry)?;
        self.expect(&Token::LBrace)?;
        let mut body = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            body.push(self.parse_func_stmt()?);
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::KwCatch)?;
        let (catch_name, catch_span) = if self.at(&Token::LBrace) {
            (None, self.peek_span())
        } else {
            let (name, span) = self.expect_ident()?;
            (Some(name), span)
        };
        self.expect(&Token::LBrace)?;
        let mut handler = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            handler.push(self.parse_func_stmt()?);
        }
        self.expect(&Token::RBrace)?;
        Ok(FuncStmt::Try(TryStmt {
            body,
            catch_name,
            catch_span,
            handler,
            span,
            end_line: self.prev_line(),
        }))
    }

    fn parse_for_stmt(&mut self) -> Result<FuncStmt, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwFor)?;
        let binding = if self.at(&Token::LParen) {
            self.advance();
            let (index_name, index_span) = self.expect_ident()?;
            self.expect(&Token::Comma)?;
            let (value_name, value_span) = self.expect_ident()?;
            self.expect(&Token::RParen)?;
            if index_name == value_name {
                return Err(SparError::ParseError {
                    message: "indexed loop bindings must use two different names".into(),
                    span: value_span,
                });
            }
            ForBinding::Indexed {
                index_name,
                index_span,
                value_name,
                value_span,
            }
        } else {
            let (name, name_span) = self.expect_ident()?;
            ForBinding::Value {
                name,
                span: name_span,
            }
        };
        self.expect(&Token::KwIn)?;
        let iterable = self.parse_expr()?;
        self.expect(&Token::LBrace)?;
        let mut body = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            body.push(self.parse_func_stmt()?);
        }
        self.expect(&Token::RBrace)?;
        Ok(FuncStmt::For(ForStmt {
            binding,
            iterable,
            body,
            span,
            end_line: self.prev_line(),
        }))
    }

    fn parse_local_var_decl(&mut self) -> Result<LocalVarDecl, SparError> {
        let span = self.peek_span();
        self.expect(&Token::Var)?;
        let mutable = if self.at(&Token::KwMut) {
            self.advance();
            true
        } else {
            false
        };
        let (name, _) = self.expect_ident()?;
        let ty = if self.at(&Token::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&Token::Eq)?;
        let value = self.parse_expr()?;
        self.expect(&Token::Semicolon)?;
        Ok(LocalVarDecl {
            name,
            mutable,
            ty,
            value,
            span,
        })
    }

    fn parse_if_stmt(&mut self) -> Result<IfStmt, SparError> {
        let span = self.peek_span();
        self.expect(&Token::KwIf)?;
        let condition = self.parse_expr()?;
        self.expect(&Token::LBrace)?;
        let mut then_stmts = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            then_stmts.push(self.parse_func_stmt()?);
        }
        self.expect(&Token::RBrace)?;
        let then_end_line = self.prev_line();
        let mut else_if = false;
        let else_stmts = if self.at(&Token::KwElse) {
            self.advance();
            if self.at(&Token::KwIf) {
                // `else if ...` is an `else` holding one nested `if`.
                else_if = true;
                let nested = self.parse_if_stmt()?;
                vec![FuncStmt::If(nested)]
            } else {
                self.expect(&Token::LBrace)?;
                let mut stmts = Vec::new();
                while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
                    stmts.push(self.parse_func_stmt()?);
                }
                self.expect(&Token::RBrace)?;
                stmts
            }
        } else {
            Vec::new()
        };
        Ok(IfStmt {
            condition,
            then_stmts,
            else_stmts,
            span,
            then_end_line,
            else_if,
            end_line: self.prev_line(),
        })
    }

    fn parse_task_decl(&mut self) -> Result<TaskDecl, SparError> {
        let span = self.peek_span();
        self.advance(); // consume the 'task' ident
        if self.at(&Token::LBracket) {
            return Err(self.error("task [Name] is removed; write task Name"));
        }
        let (name, name_span) = self.expect_ident()?;

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
        let mut run_blocks: Vec<RunBlock> = Vec::new();
        let mut seen_run_labels: std::collections::HashSet<Option<String>> =
            std::collections::HashSet::new();
        let mut field_spans: Vec<(String, Span)> = Vec::new();

        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            let (field_name, field_span) = if self.at(&Token::Private) {
                let field_span = self.peek_span();
                self.advance();
                ("private".to_string(), field_span)
            } else if self.at(&Token::TypeShell) {
                return Err(self.error(
                    "the task 'shell' field is removed; use run bash { ... } to select a shell",
                ));
            } else {
                self.expect_ident()?
            };
            if field_name != "run" {
                field_spans.push((field_name.clone(), field_span.clone()));
            }
            match field_name.as_str() {
                "run" => {
                    let (shell, shell_span, os_label, os_span) = self.parse_run_header()?;
                    let run_start = field_span.clone();
                    let body = match shell {
                        RunShell::Bash => RunBody::Bash(self.parse_run_block()?),
                        RunShell::Spar => {
                            let Expr::Shell(native) = self.parse_mixed_shell_block()? else {
                                unreachable!("mixed shell parser always returns Expr::Shell")
                            };
                            self.expect(&Token::Semicolon)?;
                            if native.steps.is_empty() && native.statements.is_empty() {
                                return Err(SparError::ParseError {
                                    message: "task 'run' block must contain at least one command"
                                        .to_string(),
                                    span: run_start,
                                });
                            }
                            RunBody::Native(native)
                        }
                    };
                    if !seen_run_labels.insert(os_label.clone()) {
                        let message = match &os_label {
                            Some(label) => format!("task 'run {label}' block may only appear once"),
                            None => "task can only have one default 'run {}' block".to_string(),
                        };
                        return Err(SparError::ParseError {
                            message,
                            span: run_start,
                        });
                    }
                    run_blocks.push(RunBlock {
                        shell,
                        shell_span,
                        os: os_label,
                        os_span,
                        body,
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
                            "unknown task field '{other}'; expected 'description', 'default', 'quiet', 'private', 'group', 'confirm', 'dependsOn', 'cwd', 'env', or 'run'"
                        ),
                        span: field_span,
                    });
                }
            }
        }
        let closing_span = self.peek_span();
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
            run_blocks,
            span,
            field_spans,
            closing_span,
        })
    }

    /// Parses the optional `[spar|bash] [linux|macos|windows]` words between
    /// `run` and its body. Shell first, OS second; both optional.
    fn parse_run_header(&mut self) -> Result<RunHeader, SparError> {
        const OS: [&str; 3] = ["linux", "macos", "windows"];
        let mut shell = RunShell::Spar;
        let mut shell_span = None;
        let mut os = None;
        let mut os_span = None;
        while let Token::Ident(word) = self.peek().clone() {
            let span = self.peek_span();
            match word.as_str() {
                "spar" | "bash" if os.is_some() => {
                    return Err(self.error("expected shell before OS: run <shell> <os> { }"));
                }
                "spar" | "bash" if shell_span.is_none() => {
                    shell = if word == "bash" {
                        RunShell::Bash
                    } else {
                        RunShell::Spar
                    };
                    shell_span = Some(span);
                }
                w if OS.contains(&w) && os.is_none() => {
                    os = Some(word.clone());
                    os_span = Some(span);
                }
                other => {
                    return Err(self.error(format!(
                        "unknown run option '{other}'; expected spar, bash, linux, macos, or windows"
                    )));
                }
            }
            self.advance();
        }
        Ok((shell, shell_span, os, os_span))
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
    fn parses_callable_type_annotations() {
        let item = first_item("var f: fn(int) -> int;");
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        assert_eq!(
            decl.ty,
            SparType::Function {
                params: vec![SparType::Int],
                return_type: Box::new(SparType::Int),
            }
        );

        let item = first_item("var map: fn(str, int) -> bool;");
        let TopLevelItem::Var(decl) = item else {
            panic!("not var")
        };
        assert_eq!(
            decl.ty,
            SparType::Function {
                params: vec![SparType::Str, SparType::Int],
                return_type: Box::new(SparType::Bool),
            }
        );
    }

    #[test]
    fn parses_expression_and_block_closures() {
        let program = parse_str(
            "function main() -> int {\n\
                 var a: fn(int) -> int = fn(x: int) -> int => x + 1;\n\
                 var b: fn(int) -> int = fn(x: int) -> int { return x + 1; };\n\
                 return 0;\n\
             };",
        );
        let TopLevelItem::Function(function) = &program.items[0] else {
            panic!("not function")
        };
        let Statement::LocalVar(a) = &function.body.stmts[0] else {
            panic!("not local var a")
        };
        let Expr::Closure {
            params,
            return_type,
            body,
            ..
        } = &a.value
        else {
            panic!("a is not closure")
        };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "x");
        assert_eq!(params[0].ty, Some(SparType::Int));
        assert_eq!(return_type.as_ref(), Some(&SparType::Int));
        assert!(matches!(body, ClosureBody::Expr(_)));

        let Statement::LocalVar(b) = &function.body.stmts[1] else {
            panic!("not local var b")
        };
        let Expr::Closure { body, .. } = &b.value else {
            panic!("b is not closure")
        };
        assert!(matches!(body, ClosureBody::Block(_)));
    }

    #[test]
    fn rejects_incomplete_callable_and_closure_syntax() {
        let err = parse_err("var f: fn(int -> int;");
        assert!(
            err.contains("expected ')'") || err.contains("found '->'"),
            "got: {err}"
        );

        let err = parse_err("var f: fn(int) int;");
        assert!(err.contains("expected '->'"), "got: {err}");

        let err = parse_err("function main() -> int { var f = fn(x: int) -> int; return 0; };");
        assert!(
            err.contains("expected '=>' or '{'") || err.contains("closure"),
            "got: {err}"
        );
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
    fn test_package_selective_import_is_explicit() {
        let item = first_item(r#"import pkg { println } from "std";"#);
        let TopLevelItem::Import(decl) = item else {
            panic!("not import")
        };
        assert!(decl.package);
        assert_eq!(decl.path, "std");
        assert!(
            matches!(decl.kind, ImportKind::Selective(ref items) if items.len() == 1 && items[0].name == "println")
        );
    }

    #[test]
    fn test_package_submodule_import_keeps_package_intent() {
        let item = first_item(r#"import pkg { createFile } from "std/fs";"#);
        let TopLevelItem::Import(decl) = item else {
            panic!("not import")
        };
        assert!(decl.package);
        assert_eq!(decl.path, "std/fs");
    }

    #[test]
    fn test_local_extensionless_import_is_not_a_package() {
        let item = first_item(r#"import { helper } from "./utils/helper";"#);
        let TopLevelItem::Import(decl) = item else {
            panic!("not import")
        };
        assert!(!decl.package);
        assert_eq!(decl.path, "./utils/helper");
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
    fn positional_call_syntax_is_parsed_before_semantic_resolution() {
        let program = parse_str(r#"var x: str = foo("bar");"#);
        let TopLevelItem::Var(var) = &program.items[0] else {
            panic!("expected var");
        };
        assert!(matches!(var.value, Some(Expr::FnCall(_))));
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
        let missing = "schema Server { port: int; }";
        assert!(parse_err(missing).contains("expected ';'"));
        parse_str("schema Server { port: int; };");
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
    fn shell_block_parses_spar_statements_and_native_commands_together() {
        let src = r#"
function install(files: [str]) -> shell {
    return shell {
        var mut installed: int = 0;
        for file in files {
            echo "Installing ${file}";
            installed = installed + 1;
        }
        if installed > 0 {
            echo "Installed ${installed} files";
        }
    };
};
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens)
            .parse()
            .expect("native shell blocks must reuse ordinary Spar statements");
    }

    #[test]
    fn shell_block_parses_multiline_lists_and_named_calls() {
        let src = r#"
function verifyZip(archive: str) -> shell {
    return shell { unzip -l "${archive}"; };
};

function main() -> shell {
    return shell {
        var files: [str] = [
            "Cargo.toml",
            "Cargo.lock",
            "src/main.rs",
        ];
        verifyZip(
            archive: files[0]
        );
    };
};
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens)
            .parse()
            .expect("multiline Spar statements inside shell blocks must parse as Spar");
    }

    #[test]
    fn shell_block_parses_multiline_native_commands_with_and_without_backslash() {
        let src = r#"
function main() -> shell {
    return shell {
        printf "%s\\n"
            one
            two;

        printf "%s\\n" \\
            three \\
            four;
    };
};
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens)
            .parse()
            .expect("native commands may span physical lines until their semicolon");
    }

    #[test]
    fn shell_background_command_can_be_followed_by_spar_and_native_statements() {
        let src = r#"
function main() -> shell {
    return shell {
        sleep 1 &
        println(message: "background started");
        echo done;
    };
};
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens)
            .parse()
            .expect("background '&' must terminate its command before the next statement");
    }

    #[test]
    fn structured_exec_requires_exactly_one_command_or_pipeline() {
        let source = "function f() -> int { var r = exec { true; false; }; return 0; };";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let error = Parser::new(tokens).parse().unwrap_err();
        assert!(
            format!("{error:?}").contains("exactly one command or pipeline"),
            "{error:?}"
        );

        let source = "function f() -> int { var r = exec { var x: int = 1; true; }; return 0; };";
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        assert!(Parser::new(tokens).parse().is_err());
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

    fn bash_commands(block: &RunBlock) -> &[ShellCommand] {
        match &block.body {
            RunBody::Bash(commands) => commands,
            RunBody::Native(_) => panic!("expected a bash run block"),
        }
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
            r#"task Build {
    run bash {
        cargo build;
    };
};"#,
        );
        assert_eq!(task.name, "Build");
        assert!(task.params.is_empty());
        assert!(task.depends_on.is_empty());
        assert_eq!(task.run_blocks.len(), 1);
        assert_eq!(bash_commands(&task.run_blocks[0]).len(), 1);
        assert!(!bash_commands(&task.run_blocks[0])[0].is_shebang);
        assert!(matches!(
            &bash_commands(&task.run_blocks[0])[0].parts[..],
            [ShellTemplatePart::Literal(s)] if s.trim() == "cargo build"
        ));
    }

    #[test]
    fn top_level_task_requires_trailing_semicolon() {
        let missing = "task Build { run { true; }; }";
        assert!(parse_err(missing).contains("expected ';'"));
        parse_str("task Build { run { true; }; };");
    }

    #[test]
    fn task_dependencies_parse() {
        let task = task_decl(
            r#"task Test {
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
            r#"task Deploy(environment: str) {
    run bash {
        ./deploy.sh ${environment};
    };
};"#,
        );
        assert_eq!(task.params.len(), 1);
        assert_eq!(task.params[0].name, "environment");
        assert_eq!(task.params[0].ty, SparType::Str);
        assert_eq!(bash_commands(&task.run_blocks[0]).len(), 1);
        assert!(matches!(
            &bash_commands(&task.run_blocks[0])[0].parts[..],
            [ShellTemplatePart::Literal(_), ShellTemplatePart::Expr(_)]
        ));
    }

    #[test]
    fn task_parameters_support_defaults_and_a_final_variadic() {
        let task = task_decl(
            r#"task Deploy(environment: str = "staging", *extra: str) {
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
            "task Deploy(*first: str, *second: str) { run { echo hi; }; }",
            "task Deploy(*extra: str, environment: str) { run { echo hi; }; }",
            "task Deploy(*extra: str = \"x\") { run { echo hi; }; }",
            "task Deploy(optional: str = \"x\", required: str) { run { echo hi; }; };",
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
            r#"task Server {
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
            r#"task Deploy {
    private: true;
    group: "release";
    confirm: "Really deploy?";
    run { ./deploy.sh; };
};"#,
        );
        assert!(matches!(
            task.private,
            Some(Expr::Literal(Literal::Bool(true)))
        ));
        assert!(task.group.is_some());
        assert!(task.confirm.is_some());
    }

    #[test]
    fn task_cwd_parses() {
        let task = task_decl(
            r#"task Web {
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
            r#"task Test {
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
            r#"task Example {
    run bash {
        echo "hello world";
        cargo test --workspace;
        npm run build;
    };
};"#,
        );
        assert_eq!(bash_commands(&task.run_blocks[0]).len(), 3);
    }

    #[test]
    fn shebang_run_block_is_one_verbatim_command() {
        let task = task_decl(
            r#"task Script {
    run bash {
        #!/usr/bin/env bash
        echo one
        echo two
        if true; then echo three; fi
    };
};"#,
        );
        assert_eq!(bash_commands(&task.run_blocks[0]).len(), 1);
        assert!(bash_commands(&task.run_blocks[0])[0].is_shebang);
        assert!(matches!(
            &bash_commands(&task.run_blocks[0])[0].parts[..],
            [ShellTemplatePart::Literal(script)]
                if script.contains("if true; then echo three; fi")
        ));
    }

    #[test]
    fn load_env_pragma_defaults_to_dotenv_and_must_be_first() {
        let program = Parser::new(
            crate::lexer::Lexer::new("@LoadEnv\ntask Build { run { echo build; }; };")
                .tokenize()
                .unwrap(),
        )
        .parse()
        .expect("@LoadEnv must parse");
        assert_eq!(program.load_env.as_deref(), Some(".env"));
        assert!(!program.is_schema_file);
        assert!(Parser::new(
            crate::lexer::Lexer::new(
                "var name: str = \"spar\";\n@LoadEnv\ntask Build { run { echo build; }; };",
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
            parse_str("@LoadEnv(\".env.production\")\ntask Build { run { echo build; }; };");
        assert_eq!(program.load_env.as_deref(), Some(".env.production"));
    }

    #[test]
    fn unknown_file_pragma_lists_supported_names() {
        let message = parse_err("@Unknown\ntask Build { run { echo build; }; };");
        assert!(
            message.contains("unknown file pragma `@Unknown`; only `@LoadEnv` is supported"),
            "got: {message}"
        );
    }

    #[test]
    fn native_shell_words_are_contextual_identifiers_outside_construct_position() {
        parse_str(
            r#"
            type [command] { value: str; };
            struct Holder {
                command: str = "field";
                exec: str = "value";
                shell: str = "name";
            };
            function command(exec: str) -> str { return exec; };
            var commandValue: str = command(exec: "ok");
            "#,
        );
    }

    #[test]
    fn task_missing_run_block_is_a_parse_error() {
        let msg = parse_err("task Build {\n};");
        assert!(msg.contains("run"), "got: {msg}");
    }

    #[test]
    fn task_unknown_field_is_a_parse_error() {
        let msg = parse_err(
            r#"task Build {
    bogus: true;
    run { echo hi; };
};"#,
        );
        assert!(msg.contains("unknown task field"), "got: {msg}");
    }

    #[test]
    fn task_without_brackets_parses_with_native_default_shell() {
        let task = task_decl("task Build { run { echo hi; }; };");
        assert_eq!(task.name, "Build");
        assert_eq!(task.run_blocks[0].shell, RunShell::Spar);
        assert!(matches!(task.run_blocks[0].body, RunBody::Native(_)));
    }

    #[test]
    fn run_header_combinations_parse() {
        let t = task_decl(
            "task T { run bash { true; }; run windows { echo w; }; run bash macos { true; }; };",
        );
        assert_eq!(t.run_blocks[0].shell, RunShell::Bash);
        assert_eq!(t.run_blocks[0].os, None);
        assert_eq!(t.run_blocks[1].shell, RunShell::Spar);
        assert_eq!(t.run_blocks[1].os.as_deref(), Some("windows"));
        assert_eq!(t.run_blocks[2].shell, RunShell::Bash);
        assert_eq!(t.run_blocks[2].os.as_deref(), Some("macos"));
    }

    #[test]
    fn removed_and_invalid_task_syntax_gives_hints() {
        assert!(parse_err("task [Build] { run { true; }; };")
            .contains("task [Name] is removed; write task Name"));
        assert!(parse_err("task B { shell: [\"sh\"]; run { true; }; };")
            .contains("the task 'shell' field is removed; use run bash { ... } to select a shell"));
        assert!(parse_err("task B { run windows bash { true; }; };")
            .contains("expected shell before OS: run <shell> <os> { }"));
        assert!(parse_err("task B { run zsh { true; }; };")
            .contains("unknown run option 'zsh'; expected spar, bash, linux, macos, or windows"));
    }

    #[test]
    fn one_run_block_per_os_slot_regardless_of_shell() {
        assert!(parse_err("task B { run { true; }; run bash { true; }; };")
            .contains("task can only have one default 'run {}' block"));
        assert!(
            parse_err("task B { run windows { true; }; run bash windows { true; }; };")
                .contains("task 'run windows' block may only appear once")
        );
    }

    #[test]
    fn task_duplicate_run_block_is_a_parse_error() {
        let msg = parse_err(
            r#"task Build {
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
        let src = "task T {\n\
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
        let src = "task T {\n\
            run windows {\n\
                echo a;\n\
            };\n\
            run windows {\n\
                echo b;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let err = crate::parser::Parser::new(tokens)
            .parse()
            .expect_err("must reject");
        let message = format!("{err}");
        assert!(message.contains("windows"), "{message}");
    }

    #[test]
    fn task_rejects_two_default_run_blocks() {
        let src = "task T {\n\
            run {\n\
                echo a;\n\
            };\n\
            run {\n\
                echo b;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let err = crate::parser::Parser::new(tokens)
            .parse()
            .expect_err("must reject");
        let message = format!("{err}");
        assert!(
            message.contains("default") || message.contains("once"),
            "{message}"
        );
    }

    #[test]
    fn task_requires_at_least_one_run_block() {
        let src = "task T {\n\
            description: \"no run at all\";\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        assert!(crate::parser::Parser::new(tokens).parse().is_err());
    }

    #[test]
    fn task_with_only_labeled_run_blocks_and_no_default_parses() {
        let src = "task T {\n\
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
