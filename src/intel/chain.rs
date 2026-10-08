//! Expression-chain helpers shared by member and scope completion: a receiver
//! `name.field[i]` as data, the local names visible at a cursor, and the type of
//! a chain. They work on the text before the cursor, so they survive syntax
//! errors elsewhere in the file.

use crate::ast::SparType;
use crate::lexer::Lexer;
use crate::resolver::{GlobalEntry, SymbolTable};

// ── Local-scope completion (token based, tolerant of broken code) ────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeNameKind {
    Parameter,
    Variable,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChainStep {
    Field(String),
    Index,
}

/// A receiver expression made only of a name, `.field` accesses and `[index]` steps.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub root: String,
    pub steps: Vec<ChainStep>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeName {
    pub name: String,
    pub kind: ScopeNameKind,
    /// Display text of the declared type, when annotated.
    pub ty: Option<String>,
    /// The declared type, parsed.
    pub declared: Option<SparType>,
    /// `var x = <chain>;` with no annotation: the type is that expression's type.
    pub init: Option<Chain>,
    /// `for x in <chain>`: the type is the element type of that expression.
    pub element_of: Option<Chain>,
}

impl ScopeName {
    pub fn new(name: &str, kind: ScopeNameKind, ty: Option<String>) -> Self {
        Self { name: name.to_string(), kind, ty, declared: None, init: None, element_of: None }
    }
}

pub(crate) fn scope_type_text(token: &crate::token::Token) -> Option<String> {
    use crate::token::Token;
    match token {
        Token::TypeStr => Some("str".into()),
        Token::TypeInt => Some("int".into()),
        Token::TypeFloat => Some("float".into()),
        Token::TypeBool => Some("bool".into()),
        Token::Ident(name) => Some(name.clone()),
        _ => None,
    }
}

/// Parses a type from tokens: `str`, `Name`, `Name<A, B>`, `[T]`.
pub fn parse_type_tokens(tokens: &[&crate::token::Token], index: &mut usize) -> Option<SparType> {
    use crate::token::Token;
    let token = tokens.get(*index).copied()?;
    let ty = match token {
        Token::LBracket => {
            *index += 1;
            let inner = parse_type_tokens(tokens, index)?;
            if tokens.get(*index).copied() != Some(&Token::RBracket) {
                return None;
            }
            *index += 1;
            return Some(SparType::List(Box::new(inner)));
        }
        Token::TypeStr => SparType::Str,
        Token::TypeInt => SparType::Int,
        Token::TypeFloat => SparType::Float,
        Token::TypeBool => SparType::Bool,
        Token::TypeShell => SparType::Shell,
        Token::TypeVoid => SparType::Void,
        Token::TypeSection => return None,
        Token::Ident(name) => {
            *index += 1;
            if tokens.get(*index).copied() == Some(&Token::Lt) {
                *index += 1;
                let mut arguments = Vec::new();
                loop {
                    arguments.push(parse_type_tokens(tokens, index)?);
                    match tokens.get(*index).copied() {
                        Some(Token::Comma) => *index += 1,
                        Some(Token::Gt) => {
                            *index += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                return Some(SparType::Applied { name: name.clone(), arguments });
            }
            return Some(SparType::Named(name.clone()));
        }
        _ => return None,
    };
    *index += 1;
    Some(ty)
}

/// Parses `name(.field | [ ... ])*` starting at `index`; only accepted when the
/// chain is the whole expression (followed by `;`, `{` or the end of input).
pub fn parse_chain_tokens(tokens: &[&crate::token::Token], mut index: usize) -> Option<Chain> {
    use crate::token::Token;
    let Token::Ident(root) = tokens.get(index).copied()? else {
        return None;
    };
    index += 1;
    let mut steps = Vec::new();
    loop {
        match tokens.get(index).copied() {
            Some(Token::Dot) => {
                let Some(Token::Ident(field)) = tokens.get(index + 1).copied() else {
                    return None;
                };
                steps.push(ChainStep::Field(field.clone()));
                index += 2;
            }
            Some(Token::LBracket) => {
                let mut depth = 0i32;
                loop {
                    match tokens.get(index).copied() {
                        Some(Token::LBracket) => depth += 1,
                        Some(Token::RBracket) => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        None | Some(Token::Eof) => return None,
                        _ => {}
                    }
                    index += 1;
                }
                steps.push(ChainStep::Index);
                index += 1;
            }
            Some(Token::Semicolon) | Some(Token::LBrace) | Some(Token::Eof) | None => break,
            _ => return None,
        }
    }
    Some(Chain { root: root.clone(), steps })
}

/// Lexes the text before the cursor. That text is usually unfinished — a
/// half-typed string or `"${`, or a `run { ... }` body still open — and a plain
/// lex would fail on it. Retry without the line being typed, and with
/// closing braces added, keeping only tokens from the real prefix.
pub(crate) fn lex_prefix_tolerantly(source: &str, end: usize) -> Option<Vec<crate::token::SpannedToken>> {
    let line_start = source[..end].rfind('\n').map_or(0, |index| index + 1);
    for cut in [end, line_start] {
        for suffix in ["", "\n}", "\n}\n}"] {
            let text = format!("{}{suffix}", &source[..cut]);
            if let Ok(mut tokens) = Lexer::new(&text).tokenize() {
                tokens.retain(|spanned| spanned.span.start < cut || spanned.token == crate::token::Token::Eof);
                return Some(tokens);
            }
        }
    }
    None
}

/// Names visible at `offset`: parameters of the enclosing function, `var`
/// declarations, `for` bindings and `catch` names in still-open blocks.
/// Works on the text before the cursor, so it survives syntax errors elsewhere.
pub fn local_names_at(source: &str, offset: usize) -> Vec<ScopeName> {
    use crate::token::Token;
    let mut end = offset.min(source.len());
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let Some(lexed) = lex_prefix_tolerantly(source, end) else {
        return Vec::new();
    };
    let tokens: Vec<&Token> = lexed.iter().map(|spanned| &spanned.token).collect();

    let mut stack: Vec<Vec<ScopeName>> = Vec::new();
    // Tracks the receiver type name for a `self` parameter: pushed/popped in
    // lockstep with `stack` at each brace, so it's `Some(owner)` exactly
    // while scanning inside `impl Owner { ... }`. `self` has no `: Type`
    // annotation in source (its type is implicit), so it can't be captured
    // by the generic `Ident ':' Type` parameter parsing below — it needs
    // this separate, explicit case.
    let mut owner_stack: Vec<Option<String>> = Vec::new();
    let mut pending_owner: Option<String> = None;
    let mut pending: Vec<ScopeName> = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        match tokens[index] {
            Token::KwImpl => {
                if let Some(Token::Ident(name)) = tokens.get(index + 1).copied() {
                    pending_owner = Some(name.clone());
                }
            }
            Token::KwFunction | Token::KwFn => {
                // function name<T>(params) -> ret {
                let mut cursor = index + 1;
                while cursor < tokens.len()
                    && *tokens[cursor] != Token::LParen
                    && *tokens[cursor] != Token::Eof
                {
                    cursor += 1;
                }
                if cursor < tokens.len() && *tokens[cursor] == Token::LParen {
                    let mut depth = 0i32;
                    let mut params = Vec::new();
                    let mut position = cursor;
                    while position < tokens.len() {
                        match tokens[position] {
                            Token::LParen => depth += 1,
                            Token::RParen => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            Token::Ident(name)
                                if depth == 1
                                    && name == "self"
                                    && (position == cursor + 1
                                        || (position == cursor + 2
                                            && tokens.get(cursor + 1).copied()
                                                == Some(&Token::KwMut))) =>
                            {
                                if let Some(Some(owner)) = owner_stack.last() {
                                    let mut param = ScopeName::new(
                                        name,
                                        ScopeNameKind::Parameter,
                                        Some(owner.clone()),
                                    );
                                    param.declared = Some(SparType::Named(owner.clone()));
                                    params.push(param);
                                }
                            }
                            Token::Ident(name)
                                if depth == 1 && tokens.get(position + 1).copied() == Some(&Token::Colon) =>
                            {
                                let ty = tokens.get(position + 2).and_then(|token| scope_type_text(token));
                                let mut type_at = position + 2;
                                let declared = parse_type_tokens(&tokens, &mut type_at);
                                let mut param = ScopeName::new(name, ScopeNameKind::Parameter, ty);
                                param.declared = declared;
                                params.push(param);
                            }
                            _ => {}
                        }
                        position += 1;
                    }
                    pending = params;
                    index = position;
                }
            }
            Token::Var | Token::KwConst => {
                let mut cursor = index + 1;
                if tokens.get(cursor).copied() == Some(&Token::KwMut) {
                    cursor += 1;
                }
                if let Some(Token::Ident(name)) = tokens.get(cursor).copied() {
                    let annotated = tokens.get(cursor + 1).copied() == Some(&Token::Colon);
                    let ty = if annotated {
                        tokens.get(cursor + 2).and_then(|token| scope_type_text(token))
                    } else {
                        None
                    };
                    let mut binding = ScopeName::new(name, ScopeNameKind::Variable, ty);
                    if annotated {
                        let mut type_at = cursor + 2;
                        binding.declared = parse_type_tokens(&tokens, &mut type_at);
                    } else if tokens.get(cursor + 1).copied() == Some(&Token::Eq) {
                        binding.init = parse_chain_tokens(&tokens, cursor + 2);
                    }
                    if let Some(scope) = stack.last_mut() {
                        scope.push(binding);
                    }
                }
            }
            Token::KwFor => {
                let mut cursor = index + 1;
                let mut bindings: Vec<ScopeName> = Vec::new();
                while cursor < tokens.len()
                    && *tokens[cursor] != Token::KwIn
                    && *tokens[cursor] != Token::LBrace
                    && *tokens[cursor] != Token::Eof
                {
                    if let Token::Ident(name) = tokens[cursor] {
                        bindings.push(ScopeName::new(name, ScopeNameKind::Variable, None));
                    }
                    cursor += 1;
                }
                if tokens.get(cursor).copied() == Some(&Token::KwIn) {
                    let iterable = parse_chain_tokens(&tokens, cursor + 1);
                    // `for x in xs` and `for (i, x) in xs`: the last name is the element.
                    if let Some(element) = bindings.last_mut() {
                        element.element_of = iterable;
                    }
                    if bindings.len() > 1 {
                        bindings[0].declared = Some(SparType::Int);
                        bindings[0].ty = Some("int".into());
                    }
                }
                pending.extend(bindings);
            }
            Token::KwCatch => {
                if let Some(Token::Ident(name)) = tokens.get(index + 1).copied() {
                    pending.push(ScopeName::new(name, ScopeNameKind::Variable, Some("error".into())));
                }
            }
            Token::LBrace => {
                stack.push(std::mem::take(&mut pending));
                // Only the impl block's own opening brace consumes
                // `pending_owner` — a nested brace (a method body, an `if`,
                // ...) inherits whatever owner is already active.
                owner_stack.push(pending_owner.take().or_else(|| owner_stack.last().cloned().flatten()));
            }
            Token::RBrace => {
                stack.pop();
                owner_stack.pop();
            }
            _ => {}
        }
        index += 1;
    }
    stack.into_iter().flatten().collect()
}

pub(crate) fn element_type(ty: &SparType) -> Option<SparType> {
    match ty {
        SparType::List(inner) => Some((**inner).clone()),
        SparType::Applied { name, arguments } if name == "List" => arguments.first().cloned(),
        SparType::Applied { name, arguments } if name == "Map" => arguments.last().cloned(),
        _ => None,
    }
}

pub(crate) fn field_type_of(symbols: &SymbolTable, ty: &SparType, field: &str) -> Option<SparType> {
    crate::SemanticSnapshot::new(symbols.clone()).field_type(ty, field)
}

pub fn type_of_chain(chain: &Chain, scope: &[ScopeName], symbols: &SymbolTable, depth: usize) -> Option<SparType> {
    if depth > 8 {
        return None;
    }
    let mut current = if let Some((index, binding)) =
        scope.iter().enumerate().rev().find(|(_, binding)| binding.name == chain.root)
    {
        // Only names declared before this one can define its type (no cycles).
        let before = &scope[..index];
        if let Some(ty) = &binding.declared {
            ty.clone()
        } else if let Some(init) = &binding.init {
            type_of_chain(init, before, symbols, depth + 1)?
        } else if let Some(iterable) = &binding.element_of {
            element_type(&type_of_chain(iterable, before, symbols, depth + 1)?)?
        } else {
            return None;
        }
    } else {
        match symbols.globals.get(&chain.root)? {
            GlobalEntry::Var { ty, .. } => ty.clone(),
            GlobalEntry::Dynamic { .. } => return None,
        }
    };
    for step in &chain.steps {
        current = match step {
            ChainStep::Field(field) => field_type_of(symbols, &current, field)?,
            ChainStep::Index => element_type(&current)?,
        };
    }
    Some(current)
}


