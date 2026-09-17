use std::collections::HashMap;

use crate::ast::{
    BinOp, Expr, FieldValue, ForBinding, FunctionDecl, Literal, ReturnValue, SectionItem,
    ShellCommandExpr, ShellExpr, ShellRedirect, ShellStep, ShellWord, ShellWordPart, SparType,
    Statement, StringPart, UnOp,
};
use crate::compiled::{
    CompiledExpression, CompiledObjectItem, CompiledShellCommand, CompiledShellExpr,
    CompiledShellRedirect, CompiledShellStep, CompiledShellWord, CompiledShellWordPart,
    CompiledStatement, CompiledStringPart, FunctionId, FunctionKey, LocalLayout, LocalSlot,
    ModuleId, TypedOperation,
};
use crate::error::{Span, SparError};
use crate::resolver::SymbolTable;
use crate::typechecker::infer_expression_with_locals;

pub(crate) fn allocate_local_layout(function: &FunctionDecl, symbols: &SymbolTable) -> LocalLayout {
    let mut allocator = LocalAllocator::new(symbols);
    let parameter_slots = function
        .params
        .iter()
        .map(|parameter| allocator.allocate(parameter.name.clone(), parameter.ty.clone()))
        .collect();
    allocator.visit_statements(&function.body.stmts);
    LocalLayout {
        parameter_slots,
        names: allocator.names,
        types: allocator.types,
    }
}

pub(crate) struct LoweringContext<'a> {
    pub module: ModuleId,
    pub imports: &'a HashMap<String, ModuleId>,
    pub functions: &'a HashMap<FunctionKey, FunctionId>,
    pub parameters: &'a HashMap<FunctionId, Vec<String>>,
}

pub(crate) struct LoweredFunction {
    pub layout: LocalLayout,
    pub defaults: Vec<Option<CompiledExpression>>,
    pub body: Vec<CompiledStatement>,
}

pub(crate) fn lower_function(
    function: &FunctionDecl,
    symbols: &SymbolTable,
    context: LoweringContext<'_>,
) -> Result<LoweredFunction, SparError> {
    let mut lowerer = FunctionLowerer {
        locals: LocalAllocator::new(symbols),
        context,
    };
    let parameter_slots = function
        .params
        .iter()
        .map(|parameter| {
            lowerer
                .locals
                .allocate(parameter.name.clone(), parameter.ty.clone())
        })
        .collect();
    let defaults = function
        .params
        .iter()
        .map(|parameter| {
            parameter
                .default
                .as_ref()
                .map(|value| lowerer.lower_expression(value))
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body = lowerer.lower_statements(&function.body.stmts)?;
    Ok(LoweredFunction {
        layout: LocalLayout {
            parameter_slots,
            names: lowerer.locals.names,
            types: lowerer.locals.types,
        },
        defaults,
        body,
    })
}

struct LocalAllocator<'a> {
    symbols: &'a SymbolTable,
    scopes: Vec<HashMap<String, (LocalSlot, SparType)>>,
    names: Vec<String>,
    types: Vec<SparType>,
}

impl<'a> LocalAllocator<'a> {
    fn new(symbols: &'a SymbolTable) -> Self {
        Self {
            symbols,
            scopes: vec![HashMap::new()],
            names: Vec::new(),
            types: Vec::new(),
        }
    }

    fn allocate(&mut self, name: String, ty: SparType) -> LocalSlot {
        let slot = LocalSlot(self.names.len() as u32);
        self.names.push(name.clone());
        self.types.push(ty.clone());
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, (slot, ty));
        }
        slot
    }

    fn lookup(&self, name: &str) -> Option<LocalSlot> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).map(|(slot, _)| *slot))
    }

    fn visible_types(&self) -> HashMap<String, SparType> {
        let mut types = HashMap::new();
        for scope in &self.scopes {
            for (name, (_, ty)) in scope {
                types.insert(name.clone(), ty.clone());
            }
        }
        types
    }

    fn expression_type(&self, expression: &Expr) -> Option<SparType> {
        infer_expression_with_locals(expression, self.symbols, &self.visible_types())
    }

    fn with_scope(&mut self, visit: impl FnOnce(&mut Self)) {
        self.scopes.push(HashMap::new());
        visit(self);
        self.scopes.pop();
    }

    fn visit_statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            match statement {
                Statement::LocalVar(local) => {
                    self.visit_expression(&local.value);
                    if let Some(ty) = local
                        .ty
                        .clone()
                        .or_else(|| self.expression_type(&local.value))
                    {
                        self.allocate(local.name.clone(), ty);
                    }
                }
                Statement::Assignment { value, .. } | Statement::Expression(value, _) => {
                    self.visit_expression(value);
                }
                Statement::If(statement) => {
                    self.visit_expression(&statement.condition);
                    self.with_scope(|this| this.visit_statements(&statement.then_stmts));
                    self.with_scope(|this| this.visit_statements(&statement.else_stmts));
                }
                Statement::Return(value, _) => self.visit_return(value),
                Statement::For(statement) => {
                    self.visit_expression(&statement.iterable);
                    let element_type = match self.expression_type(&statement.iterable) {
                        Some(SparType::List(element)) => Some(*element),
                        _ => None,
                    };
                    self.with_scope(|this| {
                        if let Some(element_type) = element_type {
                            match &statement.binding {
                                ForBinding::Value { name, .. } => {
                                    this.allocate(name.clone(), element_type);
                                }
                                ForBinding::Indexed {
                                    index_name,
                                    value_name,
                                    ..
                                } => {
                                    this.allocate(index_name.clone(), SparType::Int);
                                    this.allocate(value_name.clone(), element_type);
                                }
                            }
                        }
                        this.visit_statements(&statement.body);
                    });
                }
                Statement::Break(_) | Statement::Continue(_) => {}
                Statement::Try(_) => {}
            }
        }
    }

    fn visit_return(&mut self, value: &ReturnValue) {
        match value {
            ReturnValue::Void => {}
            ReturnValue::Expr(expression) => self.visit_expression(expression),
            ReturnValue::SectionBlock(fields) => {
                for field in fields {
                    self.visit_expression(&field.value);
                }
            }
        }
    }

    fn visit_expression(&mut self, expression: &Expr) {
        match expression {
            Expr::Comprehension {
                var_name,
                source,
                body,
                ..
            } => {
                self.visit_expression(source);
                let element_type = match self.expression_type(source) {
                    Some(SparType::List(element)) => Some(*element),
                    _ => None,
                };
                self.with_scope(|this| {
                    if let Some(element_type) = element_type {
                        this.allocate(var_name.clone(), element_type);
                    }
                    this.visit_expression(body);
                });
            }
            Expr::FnCall(call) => {
                for argument in &call.args {
                    self.visit_expression(argument);
                }
            }
            Expr::BinaryOp(operation) => {
                self.visit_expression(&operation.lhs);
                self.visit_expression(&operation.rhs);
            }
            Expr::List(items, _) => {
                for item in items {
                    self.visit_expression(item);
                }
            }
            Expr::Grouped(inner, _)
            | Expr::Unary { operand: inner, .. }
            | Expr::Await { value: inner, .. } => {
                self.visit_expression(inner);
            }
            Expr::Call { args, .. } => {
                for argument in args {
                    self.visit_expression(&argument.value);
                }
            }
            Expr::Index { source, index, .. } => {
                self.visit_expression(source);
                self.visit_expression(index);
            }
            Expr::FieldAccess { base, .. } => self.visit_expression(base),
            Expr::Object(items, _) => {
                for item in items {
                    match item {
                        SectionItem::Field(field) => match &field.value {
                            Some(FieldValue::Expr(value)) => self.visit_expression(value),
                            Some(FieldValue::Nested(_)) | None => {}
                        },
                        SectionItem::Spread(spread) => self.visit_expression(&spread.expr),
                    }
                }
            }
            Expr::String(string) => {
                for part in &string.parts {
                    if let StringPart::Expr(expression) = part {
                        self.visit_expression(expression);
                    }
                }
            }
            Expr::Literal(_)
            | Expr::NamespaceRef(_)
            | Expr::Shell(_)
            | Expr::ExecShell(_)
            | Expr::CommandSubstitution(_) => {}
        }
    }
}

struct FunctionLowerer<'a> {
    locals: LocalAllocator<'a>,
    context: LoweringContext<'a>,
}

impl FunctionLowerer<'_> {
    fn lower_statements(
        &mut self,
        statements: &[Statement],
    ) -> Result<Vec<CompiledStatement>, SparError> {
        statements
            .iter()
            .map(|statement| self.lower_statement(statement))
            .collect()
    }

    fn lower_block(
        &mut self,
        statements: &[Statement],
    ) -> Result<Vec<CompiledStatement>, SparError> {
        self.locals.scopes.push(HashMap::new());
        let result = self.lower_statements(statements);
        self.locals.scopes.pop();
        result
    }

    fn lower_statement(&mut self, statement: &Statement) -> Result<CompiledStatement, SparError> {
        Ok(match statement {
            Statement::LocalVar(local) => {
                let value = self.lower_expression(&local.value)?;
                let ty = local
                    .ty
                    .clone()
                    .or_else(|| self.locals.expression_type(&local.value))
                    .ok_or_else(|| internal_lowering("missing checked local type", &local.span))?;
                let slot = self.locals.allocate(local.name.clone(), ty);
                CompiledStatement::StoreLocal {
                    slot,
                    value,
                    span: local.span.clone(),
                }
            }
            Statement::Assignment { name, value, span } => {
                let value = self.lower_expression(value)?;
                match self.locals.lookup(name) {
                    Some(slot) => CompiledStatement::StoreLocal {
                        slot,
                        value,
                        span: span.clone(),
                    },
                    None => CompiledStatement::StoreGlobal {
                        name: name.clone(),
                        value,
                        span: span.clone(),
                    },
                }
            }
            Statement::Expression(expression, span) => {
                CompiledStatement::Expression(self.lower_expression(expression)?, span.clone())
            }
            Statement::If(statement) => CompiledStatement::If {
                condition: self.lower_expression(&statement.condition)?,
                then_body: self.lower_block(&statement.then_stmts)?,
                else_body: self.lower_block(&statement.else_stmts)?,
                span: statement.span.clone(),
            },
            Statement::Return(value, span) => {
                let value = match value {
                    ReturnValue::Void => None,
                    ReturnValue::Expr(expression) => Some(self.lower_expression(expression)?),
                    ReturnValue::SectionBlock(fields) => Some(CompiledExpression::Object(
                        fields
                            .iter()
                            .map(|field| {
                                Ok(CompiledObjectItem::Field {
                                    name: field.name.clone(),
                                    value: self.lower_expression(&field.value)?,
                                })
                            })
                            .collect::<Result<Vec<_>, SparError>>()?,
                        span.clone(),
                    )),
                };
                CompiledStatement::Return(value, span.clone())
            }
            Statement::For(statement) => {
                let iterable = self.lower_expression(&statement.iterable)?;
                let element_type = match self.locals.expression_type(&statement.iterable) {
                    Some(SparType::List(element)) => *element,
                    _ => {
                        return Err(internal_lowering(
                            "missing checked loop type",
                            &statement.span,
                        ))
                    }
                };
                self.locals.scopes.push(HashMap::new());
                let (index_slot, value_slot) = match &statement.binding {
                    ForBinding::Value { name, .. } => {
                        (None, self.locals.allocate(name.clone(), element_type))
                    }
                    ForBinding::Indexed {
                        index_name,
                        value_name,
                        ..
                    } => (
                        Some(self.locals.allocate(index_name.clone(), SparType::Int)),
                        self.locals.allocate(value_name.clone(), element_type),
                    ),
                };
                let body = self.lower_statements(&statement.body);
                self.locals.scopes.pop();
                CompiledStatement::For {
                    index_slot,
                    value_slot,
                    iterable,
                    body: body?,
                    span: statement.span.clone(),
                }
            }
            Statement::Break(span) => CompiledStatement::Break(span.clone()),
            Statement::Continue(span) => CompiledStatement::Continue(span.clone()),
            Statement::Try(ts) => {
                self.locals.scopes.push(HashMap::new());
                let catch_slot = ts
                    .catch_name
                    .as_ref()
                    .map(|name| self.locals.allocate(name.clone(), SparType::Error));
                let handler = self.lower_statements(&ts.handler)?;
                self.locals.scopes.pop();
                CompiledStatement::Try {
                    body: self.lower_statements(&ts.body)?,
                    catch_slot,
                    handler,
                    span: ts.span.clone(),
                }
            }
        })
    }

    fn lower_expression(&mut self, expression: &Expr) -> Result<CompiledExpression, SparError> {
        let span = expression_span(expression);
        Ok(match expression {
            Expr::Literal(value) => CompiledExpression::Constant(
                match value {
                    Literal::Int(value) => crate::ConfigValue::Int(*value),
                    Literal::Float(value) => crate::ConfigValue::Float(*value),
                    Literal::Bool(value) => crate::ConfigValue::Bool(*value),
                },
                span,
            ),
            Expr::String(string) => CompiledExpression::Interpolation(
                string
                    .parts
                    .iter()
                    .map(|part| match part {
                        StringPart::Literal(value) => {
                            Ok(CompiledStringPart::Literal(value.clone()))
                        }
                        StringPart::Expr(value) => Ok(CompiledStringPart::Expression(
                            self.lower_expression(value)?,
                        )),
                    })
                    .collect::<Result<Vec<_>, SparError>>()?,
                string.span.clone(),
            ),
            Expr::NamespaceRef(reference) => {
                if reference.segments.len() == 1 {
                    if let Some(slot) = self.locals.lookup(&reference.segments[0]) {
                        CompiledExpression::Local(slot, reference.span.clone())
                    } else if self
                        .locals
                        .symbols
                        .lookup_section(&reference.segments)
                        .is_some()
                    {
                        CompiledExpression::ImportedValue {
                            module: self.context.module,
                            path: reference.segments.clone(),
                            span: reference.span.clone(),
                        }
                    } else {
                        CompiledExpression::Global(
                            reference.segments[0].clone(),
                            reference.span.clone(),
                        )
                    }
                } else if let Some(module) = self.context.imports.get(&reference.segments[0]) {
                    CompiledExpression::ImportedValue {
                        module: *module,
                        path: reference.segments[1..].to_vec(),
                        span: reference.span.clone(),
                    }
                } else {
                    CompiledExpression::Global(
                        reference.segments.join("::"),
                        reference.span.clone(),
                    )
                }
            }
            Expr::Call {
                name, args, span, ..
            } => self.lower_call(name, args, span)?,
            Expr::FnCall(call) => CompiledExpression::HostCall {
                namespace: String::new(),
                name: call.name.clone(),
                arguments: call
                    .args
                    .iter()
                    .map(|argument| self.lower_expression(argument))
                    .collect::<Result<Vec<_>, _>>()?,
                span: call.span.clone(),
            },
            Expr::BinaryOp(binary) => {
                let left_type = self.locals.expression_type(&binary.lhs).ok_or_else(|| {
                    internal_lowering("missing checked operand type", &binary.span)
                })?;
                CompiledExpression::Operation {
                    operation: typed_binary(&binary.op, &left_type).ok_or_else(|| {
                        internal_lowering("unsupported checked operation", &binary.span)
                    })?,
                    operands: vec![
                        self.lower_expression(&binary.lhs)?,
                        self.lower_expression(&binary.rhs)?,
                    ],
                    span: binary.span.clone(),
                }
            }
            Expr::Unary { op, operand, span } => {
                let operand_type = self
                    .locals
                    .expression_type(operand)
                    .ok_or_else(|| internal_lowering("missing checked operand type", span))?;
                CompiledExpression::Operation {
                    operation: typed_unary(op, &operand_type)
                        .ok_or_else(|| internal_lowering("unsupported checked operation", span))?,
                    operands: vec![self.lower_expression(operand)?],
                    span: span.clone(),
                }
            }
            Expr::Await { value, span } => CompiledExpression::Await {
                promise: Box::new(self.lower_expression(value)?),
                span: span.clone(),
            },
            Expr::List(items, span) => CompiledExpression::List(
                items
                    .iter()
                    .map(|item| self.lower_expression(item))
                    .collect::<Result<Vec<_>, _>>()?,
                span.clone(),
            ),
            Expr::Grouped(inner, _) => self.lower_expression(inner)?,
            Expr::Index {
                source,
                index,
                span,
            } => CompiledExpression::Index {
                source: Box::new(self.lower_expression(source)?),
                index: Box::new(self.lower_expression(index)?),
                span: span.clone(),
            },
            Expr::FieldAccess {
                base, field, span, ..
            } => CompiledExpression::Field {
                base: Box::new(self.lower_expression(base)?),
                field: field.clone(),
                span: span.clone(),
            },
            Expr::Comprehension {
                var_name,
                source,
                body,
                span,
                ..
            } => {
                let source_type = self.locals.expression_type(source);
                let Some(SparType::List(element_type)) = source_type else {
                    return Err(internal_lowering(
                        "missing checked comprehension type",
                        span,
                    ));
                };
                let source = Box::new(self.lower_expression(source)?);
                self.locals.scopes.push(HashMap::new());
                let binding = self.locals.allocate(var_name.clone(), *element_type);
                let body = self.lower_expression(body);
                self.locals.scopes.pop();
                CompiledExpression::Comprehension {
                    binding,
                    source,
                    body: Box::new(body?),
                    span: span.clone(),
                }
            }
            Expr::Object(items, span) => {
                CompiledExpression::Object(self.lower_object_items(items)?, span.clone())
            }
            Expr::Shell(shell) if shell.statements.is_empty() => {
                CompiledExpression::Shell(self.lower_shell(shell)?)
            }
            Expr::Shell(shell) => CompiledExpression::ShellProgram {
                body: self.lower_block(&shell.statements)?,
                span: shell.span.clone(),
            },
            Expr::ExecShell(shell) => CompiledExpression::ExecShell(shell.clone()),
            Expr::CommandSubstitution(shell) => {
                CompiledExpression::CommandSubstitution(self.lower_shell(shell)?)
            }
        })
    }

    fn lower_shell(&mut self, shell: &ShellExpr) -> Result<CompiledShellExpr, SparError> {
        Ok(CompiledShellExpr {
            steps: shell
                .steps
                .iter()
                .map(|(join, step)| {
                    Ok((
                        join.clone(),
                        match step {
                            ShellStep::Command(command) => CompiledShellStep::Command(Box::new(
                                self.lower_shell_command(command)?,
                            )),
                            ShellStep::Pipeline(commands) => CompiledShellStep::Pipeline(
                                commands
                                    .iter()
                                    .map(|command| self.lower_shell_command(command))
                                    .collect::<Result<Vec<_>, SparError>>()?,
                            ),
                        },
                    ))
                })
                .collect::<Result<Vec<_>, SparError>>()?,
            span: shell.span.clone(),
        })
    }

    fn lower_shell_command(
        &mut self,
        command: &ShellCommandExpr,
    ) -> Result<CompiledShellCommand, SparError> {
        Ok(CompiledShellCommand {
            environment: command
                .environment
                .iter()
                .map(|entry| (entry.name.clone(), entry.value.clone()))
                .collect(),
            program: self.lower_shell_word(&command.program)?,
            args: command
                .args
                .iter()
                .map(|word| self.lower_shell_word(word))
                .collect::<Result<Vec<_>, SparError>>()?,
            stdin: command
                .stdin
                .as_ref()
                .map(|redirect| self.lower_shell_redirect(redirect))
                .transpose()?,
            stdout: command
                .stdout
                .as_ref()
                .map(|redirect| self.lower_shell_redirect(redirect))
                .transpose()?,
            stderr: command
                .stderr
                .as_ref()
                .map(|redirect| self.lower_shell_redirect(redirect))
                .transpose()?,
        })
    }

    fn lower_shell_redirect(
        &mut self,
        redirect: &ShellRedirect,
    ) -> Result<CompiledShellRedirect, SparError> {
        Ok(CompiledShellRedirect {
            target: self.lower_shell_word(&redirect.target)?,
            mode: redirect.mode.clone(),
        })
    }

    fn lower_shell_word(&mut self, word: &ShellWord) -> Result<CompiledShellWord, SparError> {
        Ok(CompiledShellWord {
            parts: word
                .parts
                .iter()
                .map(|part| match part {
                    ShellWordPart::Literal(value) => {
                        Ok(CompiledShellWordPart::Literal(value.clone()))
                    }
                    ShellWordPart::Expr(expression) => Ok(CompiledShellWordPart::Expression(
                        self.lower_expression(expression)?,
                    )),
                    ShellWordPart::Environment(name) => {
                        Ok(CompiledShellWordPart::Environment(name.clone()))
                    }
                })
                .collect::<Result<Vec<_>, SparError>>()?,
            span: word.span.clone(),
        })
    }

    fn lower_call(
        &mut self,
        name: &str,
        arguments: &[crate::ast::CallArg],
        span: &Span,
    ) -> Result<CompiledExpression, SparError> {
        if name == "panic" {
            let message = arguments
                .iter()
                .find(|argument| argument.param_name == "message")
                .ok_or_else(|| internal_lowering("panic message argument is unavailable", span))?;
            return Ok(CompiledExpression::Panic {
                message: Box::new(self.lower_expression(&message.value)?),
                span: span.clone(),
            });
        }
        let segments: Vec<&str> = name.split("::").collect();
        let key = match segments.as_slice() {
            [function] => Some(FunctionKey {
                module: self.context.module,
                group: None,
                name: (*function).to_string(),
            }),
            [namespace, function] => {
                let local_group = FunctionKey {
                    module: self.context.module,
                    group: Some((*namespace).to_string()),
                    name: (*function).to_string(),
                };
                if self.context.functions.contains_key(&local_group) {
                    Some(local_group)
                } else {
                    self.context
                        .imports
                        .get(*namespace)
                        .map(|module| FunctionKey {
                            module: *module,
                            group: None,
                            name: (*function).to_string(),
                        })
                }
            }
            [alias, group, function] => {
                self.context.imports.get(*alias).map(|module| FunctionKey {
                    module: *module,
                    group: Some((*group).to_string()),
                    name: (*function).to_string(),
                })
            }
            _ => None,
        };
        if let Some(function) = key.and_then(|key| self.context.functions.get(&key).copied()) {
            let parameter_names =
                self.context.parameters.get(&function).ok_or_else(|| {
                    internal_lowering("direct call target has no signature", span)
                })?;
            let mut ordered = Vec::new();
            for parameter_name in parameter_names {
                if let Some(argument) = arguments
                    .iter()
                    .find(|argument| &argument.param_name == parameter_name)
                {
                    ordered.push(self.lower_expression(&argument.value)?);
                }
            }
            return Ok(CompiledExpression::DirectCall {
                function,
                arguments: ordered,
                span: span.clone(),
            });
        }
        let (namespace, function_name) = segments.split_at(segments.len().saturating_sub(1));
        Ok(CompiledExpression::HostCall {
            namespace: namespace.join("::"),
            name: function_name.first().copied().unwrap_or(name).to_string(),
            arguments: arguments
                .iter()
                .map(|argument| self.lower_expression(&argument.value))
                .collect::<Result<Vec<_>, _>>()?,
            span: span.clone(),
        })
    }

    fn lower_object_items(
        &mut self,
        items: &[SectionItem],
    ) -> Result<Vec<CompiledObjectItem>, SparError> {
        items
            .iter()
            .map(|item| match item {
                SectionItem::Spread(spread) => Ok(CompiledObjectItem::Spread(
                    self.lower_expression(&spread.expr)?,
                )),
                SectionItem::Field(field) => {
                    let value = match &field.value {
                        Some(FieldValue::Expr(value)) => self.lower_expression(value)?,
                        Some(FieldValue::Nested(items)) => CompiledExpression::Object(
                            self.lower_object_items(items)?,
                            field.span.clone(),
                        ),
                        None => {
                            return Err(internal_lowering(
                                "checked object field has no value",
                                &field.span,
                            ))
                        }
                    };
                    Ok(CompiledObjectItem::Field {
                        name: field.name.clone(),
                        value,
                    })
                }
            })
            .collect()
    }
}

fn typed_binary(operation: &BinOp, operand: &SparType) -> Option<TypedOperation> {
    Some(match (operation, operand) {
        (BinOp::Add, SparType::Int) => TypedOperation::IntAdd,
        (BinOp::Add, SparType::Float) => TypedOperation::FloatAdd,
        (BinOp::Add, SparType::Str) => TypedOperation::StringConcat,
        (BinOp::Add, SparType::Shell) => TypedOperation::ShellConcat,
        (BinOp::Sub, SparType::Int) => TypedOperation::IntSub,
        (BinOp::Sub, SparType::Float) => TypedOperation::FloatSub,
        (BinOp::Mul, SparType::Int) => TypedOperation::IntMul,
        (BinOp::Mul, SparType::Float) => TypedOperation::FloatMul,
        (BinOp::Div, SparType::Int) => TypedOperation::IntDiv,
        (BinOp::Div, SparType::Float) => TypedOperation::FloatDiv,
        (BinOp::Eq, SparType::Int) => TypedOperation::IntEq,
        (BinOp::Eq, SparType::Float) => TypedOperation::FloatEq,
        (BinOp::Eq, SparType::Str) => TypedOperation::StringEq,
        (BinOp::Eq, SparType::Bool) => TypedOperation::BoolEq,
        (BinOp::NotEq, SparType::Int) => TypedOperation::IntNotEq,
        (BinOp::NotEq, SparType::Float) => TypedOperation::FloatNotEq,
        (BinOp::NotEq, SparType::Str) => TypedOperation::StringNotEq,
        (BinOp::NotEq, SparType::Bool) => TypedOperation::BoolNotEq,
        (BinOp::Lt, SparType::Int) => TypedOperation::IntLt,
        (BinOp::Lt, SparType::Float) => TypedOperation::FloatLt,
        (BinOp::Gt, SparType::Int) => TypedOperation::IntGt,
        (BinOp::Gt, SparType::Float) => TypedOperation::FloatGt,
        (BinOp::LtEq, SparType::Int) => TypedOperation::IntLtEq,
        (BinOp::LtEq, SparType::Float) => TypedOperation::FloatLtEq,
        (BinOp::GtEq, SparType::Int) => TypedOperation::IntGtEq,
        (BinOp::GtEq, SparType::Float) => TypedOperation::FloatGtEq,
        (BinOp::And, SparType::Bool) => TypedOperation::BoolAnd,
        (BinOp::Or, SparType::Bool) => TypedOperation::BoolOr,
        (BinOp::Fallback, _) => TypedOperation::Fallback,
        _ => return None,
    })
}

fn typed_unary(operation: &UnOp, operand: &SparType) -> Option<TypedOperation> {
    match (operation, operand) {
        (UnOp::Not, SparType::Bool) => Some(TypedOperation::BoolNot),
        (UnOp::Neg, SparType::Int) => Some(TypedOperation::IntNeg),
        (UnOp::Neg, SparType::Float) => Some(TypedOperation::FloatNeg),
        _ => None,
    }
}

fn expression_span(expression: &Expr) -> Span {
    match expression {
        Expr::Literal(_) => Span::dummy(),
        Expr::String(value) => value.span.clone(),
        Expr::NamespaceRef(value) => value.span.clone(),
        Expr::FnCall(value) => value.span.clone(),
        Expr::BinaryOp(value) => value.span.clone(),
        Expr::List(_, span)
        | Expr::Grouped(_, span)
        | Expr::Call { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Await { span, .. }
        | Expr::Comprehension { span, .. }
        | Expr::Index { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::Object(_, span) => span.clone(),
        Expr::Shell(value) | Expr::ExecShell(value) | Expr::CommandSubstitution(value) => {
            value.span.clone()
        }
    }
}

fn internal_lowering(message: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("internal lowering error: {message}"),
        span: span.clone(),
    }
}
