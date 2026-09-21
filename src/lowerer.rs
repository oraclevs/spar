use std::collections::HashMap;

use crate::ast::{
    BinOp, ClosureBody, Expr, FieldValue, ForBinding, FunctionDecl, Literal, ReturnValue,
    SectionItem, ShellCommandExpr, ShellExpr, ShellFdRedirectTarget, ShellRedirect, ShellStep,
    ShellWord, ShellWordPart, SparType, Statement, StringPart, UnOp,
};
use crate::compiled::{
    CompiledDecoderArg, CompiledExpression, CompiledMethodTarget, CompiledObjectItem,
    CompiledShellCommand, CompiledShellDecodeStage, CompiledShellExpr, CompiledShellMixedPipeline,
    CompiledShellRedirect, CompiledShellStep, CompiledShellStructuredStage, CompiledShellWord,
    CompiledShellWordPart, CompiledStatement, CompiledStringPart, FunctionId, FunctionKey,
    LocalLayout, LocalSlot, ModuleId, TypedOperation,
};
use crate::error::{Span, SparError};
use crate::resolver::SymbolTable;
use crate::typechecker::{
    infer_expression_with_locals, substitute_type, unify_generic, TypeSubstitution,
};

fn method_owner_name(ty: &SparType) -> Option<String> {
    match ty {
        SparType::Named(name) => Some(name.clone()),
        SparType::Applied { name, .. } => Some(name.clone()),
        SparType::Str => Some("str".into()),
        SparType::List(_) => Some("List".into()),
        _ => None,
    }
}

fn mixed_decoder_stream_type(decoder: &crate::ast::ShellDecodeStage) -> SparType {
    let registry = crate::structured_input::StructuredInputRegistry::builtin();
    let element = registry
        .resolve(
            decoder.decoder.namespace,
            &decoder.decoder.name,
            &decoder.decoder.span,
        )
        .ok()
        .map(|resolved| {
            use crate::structured_input::DecoderOutputShape;
            match resolved
                .descriptor
                .stream_item
                .unwrap_or(resolved.descriptor.normalized_output)
            {
                DecoderOutputShape::Scalar => SparType::Str,
                DecoderOutputShape::List => {
                    SparType::List(Box::new(SparType::Named("Record".into())))
                }
                DecoderOutputShape::Record | DecoderOutputShape::Table => {
                    SparType::Named("Record".into())
                }
            }
        })
        .unwrap_or_else(|| SparType::Named("Record".into()));
    SparType::Applied {
        name: "Stream".into(),
        arguments: vec![element],
    }
}

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

#[derive(Clone, Copy)]
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
        return_type: function.ret.clone(),
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

    fn visible_bindings(&self) -> Vec<(String, LocalSlot, SparType)> {
        let mut bindings = HashMap::<String, (LocalSlot, SparType)>::new();
        for scope in &self.scopes {
            for (name, (slot, ty)) in scope {
                bindings.insert(name.clone(), (*slot, ty.clone()));
            }
        }
        bindings
            .into_iter()
            .map(|(name, (slot, ty))| (name, slot, ty))
            .collect()
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
                Statement::Assignment { value, .. }
                | Statement::FieldAssignment { value, .. }
                | Statement::Expression(value, _) => {
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
            Expr::Closure { params, body, .. } => {
                self.with_scope(|this| {
                    for param in params {
                        if let Some(ty) = &param.ty {
                            this.allocate(param.name.clone(), ty.clone());
                        }
                    }
                    match body {
                        ClosureBody::Expr(value) => this.visit_expression(value),
                        ClosureBody::Block(body) => this.visit_statements(&body.stmts),
                    }
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
            Expr::MethodCall { receiver, args, .. } => {
                self.visit_expression(receiver);
                for argument in args {
                    self.visit_expression(argument);
                }
            }
            Expr::StructuredPipe { input, stage, .. } => {
                self.visit_expression(input);
                self.visit_expression(stage);
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
    return_type: SparType,
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
                let ty = local
                    .ty
                    .clone()
                    .or_else(|| self.locals.expression_type(&local.value))
                    .ok_or_else(|| internal_lowering("missing checked local type", &local.span))?;
                let value = self.lower_expression_expected(&local.value, Some(&ty))?;
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
            Statement::FieldAssignment {
                base,
                fields,
                value,
                span,
            } => {
                let value = self.lower_expression(value)?;
                match self.locals.lookup(base) {
                    Some(slot) => CompiledStatement::StoreFieldLocal {
                        slot,
                        fields: fields.clone(),
                        value,
                        span: span.clone(),
                    },
                    None => CompiledStatement::StoreFieldGlobal {
                        name: base.clone(),
                        fields: fields.clone(),
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
                    ReturnValue::Expr(expression) => {
                        let expected = self.return_type.clone();
                        Some(self.lower_expression_expected(expression, Some(&expected))?)
                    }
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
        self.lower_expression_expected(expression, None)
    }

    fn lower_expression_expected(
        &mut self,
        expression: &Expr,
        expected: Option<&SparType>,
    ) -> Result<CompiledExpression, SparError> {
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
                        let key = FunctionKey {
                            module: self.context.module,
                            group: None,
                            name: reference.segments[0].clone(),
                        };
                        if let Some(function) = self.context.functions.get(&key).copied() {
                            CompiledExpression::FunctionRef {
                                function,
                                span: reference.span.clone(),
                            }
                        } else {
                            CompiledExpression::Global(
                                reference.segments[0].clone(),
                                reference.span.clone(),
                            )
                        }
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
            Expr::Closure {
                params,
                return_type,
                body,
                span,
            } => self.lower_closure(params, return_type.as_ref(), body, span, expected)?,
            Expr::Call {
                name, args, span, ..
            } => self.lower_call(name, args, span)?,
            Expr::FnCall(call) => {
                if call.args.is_empty()
                    && self
                        .locals
                        .symbols
                        .lookup_section(std::slice::from_ref(&call.name))
                        .is_some_and(|section| section.canonical)
                {
                    return Ok(CompiledExpression::StructConstruct {
                        module: self.context.module,
                        name: call.name.clone(),
                        overrides: Vec::new(),
                        span: call.span.clone(),
                    });
                }
                let arguments = call
                    .args
                    .iter()
                    .map(|argument| self.lower_expression(argument))
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(slot) = self.locals.lookup(&call.name) {
                    CompiledExpression::Invoke {
                        callee: Box::new(CompiledExpression::Local(slot, call.span.clone())),
                        arguments,
                        span: call.span.clone(),
                    }
                } else {
                    let key = FunctionKey {
                        module: self.context.module,
                        group: None,
                        name: call.name.clone(),
                    };
                    if let Some(function) = self.context.functions.get(&key).copied() {
                        CompiledExpression::Invoke {
                            callee: Box::new(CompiledExpression::FunctionRef {
                                function,
                                span: call.span.clone(),
                            }),
                            arguments,
                            span: call.span.clone(),
                        }
                    } else {
                        CompiledExpression::HostCall {
                            namespace: String::new(),
                            name: call.name.clone(),
                            arguments,
                            span: call.span.clone(),
                        }
                    }
                }
            }
            Expr::StructuredPipe { input, stage, span } => {
                let input_value = self.lower_expression(input)?;
                match stage.as_ref() {
                    Expr::FnCall(call) => {
                        let mut arguments = Vec::with_capacity(call.args.len() + 1);
                        arguments.push(input_value);
                        // Closures with untyped parameters get them from the
                        // stage signature instantiated for the piped input.
                        let inferred = if call.args.iter().any(is_untyped_closure) {
                            crate::typechecker::pipe_stage_parameters_with_locals(
                                input,
                                stage,
                                self.locals.symbols,
                                &self.locals.visible_types(),
                            )
                        } else {
                            None
                        };
                        for (index, argument) in call.args.iter().enumerate() {
                            let expected = inferred
                                .as_ref()
                                .and_then(|parameters| parameters.get(index + 1))
                                .map(|(_, ty)| ty);
                            arguments.push(self.lower_expression_expected(argument, expected)?);
                        }
                        let callee = if let Some(slot) = self.locals.lookup(&call.name) {
                            CompiledExpression::Local(slot, call.span.clone())
                        } else {
                            let key = FunctionKey {
                                module: self.context.module,
                                group: None,
                                name: call.name.clone(),
                            };
                            if let Some(function) = self.context.functions.get(&key).copied() {
                                CompiledExpression::FunctionRef {
                                    function,
                                    span: call.span.clone(),
                                }
                            } else {
                                return Err(internal_lowering(
                                    "structured pipe stage is not a checked callable",
                                    span,
                                ));
                            }
                        };
                        CompiledExpression::Invoke {
                            callee: Box::new(callee),
                            arguments,
                            span: span.clone(),
                        }
                    }
                    Expr::Call {
                        name,
                        args,
                        span: call_span,
                        ..
                    } => {
                        let entry = self.locals.symbols.lookup_function(name).ok_or_else(|| {
                            internal_lowering(
                                "structured pipe named stage has no local function signature",
                                span,
                            )
                        })?;
                        let first = entry.params.first().ok_or_else(|| {
                            internal_lowering("structured pipe stage has no first parameter", span)
                        })?;
                        let mut injected = Vec::with_capacity(args.len() + 1);
                        injected.push(crate::ast::CallArg {
                            param_name: first.0.clone(),
                            param_name_span: span.clone(),
                            value: input.as_ref().clone(),
                            span: span.clone(),
                        });
                        injected.extend(args.iter().cloned());
                        if args.iter().any(|arg| is_untyped_closure(&arg.value)) {
                            // Give each untyped closure its inferred type by
                            // spelling it out, so `lower_call` sees a typed closure.
                            let inferred = crate::typechecker::pipe_stage_parameters_with_locals(
                                input,
                                stage,
                                self.locals.symbols,
                                &self.locals.visible_types(),
                            );
                            for arg in injected.iter_mut() {
                                if let (true, Some(parameters)) =
                                    (is_untyped_closure(&arg.value), inferred.as_ref())
                                {
                                    if let Some((_, SparType::Function { params, .. })) = parameters
                                        .iter()
                                        .find(|(param, _)| param == &arg.param_name)
                                    {
                                        annotate_closure_parameters(&mut arg.value, params);
                                    }
                                }
                            }
                        }
                        self.lower_call(name, &injected, call_span)?
                    }
                    _ => CompiledExpression::Invoke {
                        callee: Box::new(self.lower_expression(stage)?),
                        arguments: vec![input_value],
                        span: span.clone(),
                    },
                }
            }
            Expr::BinaryOp(binary) => {
                let left_type = self.locals.expression_type(&binary.lhs).ok_or_else(|| {
                    internal_lowering("missing checked operand type", &binary.span)
                })?;
                let is_dynamic =
                    |ty: &SparType| matches!(ty, SparType::Named(name) if name == "Record");
                let dynamic_equality = matches!(
                    binary.op,
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq
                ) && (is_dynamic(&left_type)
                    || self
                        .locals
                        .expression_type(&binary.rhs)
                        .is_some_and(|ty| is_dynamic(&ty)));
                CompiledExpression::Operation {
                    operation: if dynamic_equality {
                        match binary.op {
                            BinOp::Eq => TypedOperation::DynamicEq,
                            BinOp::NotEq => TypedOperation::DynamicNotEq,
                            BinOp::Lt => TypedOperation::DynamicLt,
                            BinOp::Gt => TypedOperation::DynamicGt,
                            BinOp::LtEq => TypedOperation::DynamicLtEq,
                            _ => TypedOperation::DynamicGtEq,
                        }
                    } else {
                        typed_binary(&binary.op, &left_type).ok_or_else(|| {
                            internal_lowering("unsupported checked operation", &binary.span)
                        })?
                    },
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
            Expr::MethodCall {
                receiver,
                method,
                args,
                span,
                ..
            } => {
                let (owner, is_static) = if let Expr::NamespaceRef(reference) = receiver.as_ref() {
                    if reference.segments.len() == 1
                        && self
                            .locals
                            .symbols
                            .lookup_section(&reference.segments)
                            .is_some_and(|section| section.canonical)
                    {
                        (reference.segments[0].clone(), true)
                    } else {
                        let ty = self.locals.expression_type(receiver).ok_or_else(|| {
                            internal_lowering("missing checked method receiver type", span)
                        })?;
                        (
                            method_owner_name(&ty).ok_or_else(|| {
                                internal_lowering("unsupported method receiver type", span)
                            })?,
                            false,
                        )
                    }
                } else {
                    let ty = self.locals.expression_type(receiver).ok_or_else(|| {
                        internal_lowering("missing checked method receiver type", span)
                    })?;
                    (
                        method_owner_name(&ty).ok_or_else(|| {
                            internal_lowering("unsupported method receiver type", span)
                        })?,
                        false,
                    )
                };
                let entry = self
                    .locals
                    .symbols
                    .lookup_method(&owner, method)
                    .ok_or_else(|| internal_lowering("missing checked method metadata", span))?;
                if is_static == entry.has_receiver {
                    return Err(internal_lowering(
                        "checked method receiver/static mismatch",
                        span,
                    ));
                }
                let target = if let Some(native_method) = entry.native_method {
                    CompiledMethodTarget::Native(native_method)
                } else {
                    let key = FunctionKey {
                        module: self.context.module,
                        group: Some(format!("impl:{owner}")),
                        name: method.clone(),
                    };
                    let function = self.context.functions.get(&key).copied().ok_or_else(|| {
                        internal_lowering("missing compiled method function", span)
                    })?;
                    CompiledMethodTarget::Function(function)
                };
                let parameter_types = if entry.has_receiver {
                    &entry.function.params[1..]
                } else {
                    &entry.function.params[..]
                };
                let mut substitution = TypeSubstitution::new();
                if entry.has_receiver {
                    let receiver_type = self.locals.expression_type(receiver).ok_or_else(|| {
                        internal_lowering("missing checked method receiver type", span)
                    })?;
                    let (_, expected_receiver) =
                        entry.function.params.first().ok_or_else(|| {
                            internal_lowering("method receiver metadata is missing", span)
                        })?;
                    unify_generic(expected_receiver, &receiver_type, &mut substitution, span)?;
                }
                let arguments = args
                    .iter()
                    .zip(parameter_types.iter())
                    .map(|(argument, (_, expected))| {
                        let expected = substitute_type(expected, &substitution);
                        self.lower_expression_expected(argument, Some(&expected))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let (receiver_expr, receiver_slot, receiver_global) = if entry.has_receiver {
                    let slot = if entry.receiver_mutable {
                        if let Expr::NamespaceRef(reference) = receiver.as_ref() {
                            if reference.segments.len() == 1 {
                                self.locals.lookup(&reference.segments[0])
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let global = if entry.receiver_mutable && slot.is_none() {
                        if let Expr::NamespaceRef(reference) = receiver.as_ref() {
                            if reference.segments.len() == 1 {
                                Some(reference.segments[0].clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if entry.receiver_mutable && slot.is_none() && global.is_none() {
                        return Err(internal_lowering(
                            "mutable method receiver must be a binding",
                            span,
                        ));
                    }
                    (
                        Some(Box::new(self.lower_expression(receiver)?)),
                        slot,
                        global,
                    )
                } else {
                    (None, None, None)
                };
                CompiledExpression::MethodCall {
                    target,
                    receiver: receiver_expr,
                    receiver_slot,
                    receiver_global,
                    arguments,
                    mutates_receiver: entry.receiver_mutable,
                    span: span.clone(),
                }
            }
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
            Expr::Shell(shell)
                if shell.statements.is_empty()
                    && !shell
                        .steps
                        .iter()
                        .any(|(_, step)| matches!(step, ShellStep::MixedPipeline(_))) =>
            {
                CompiledExpression::Shell(self.lower_shell(shell)?)
            }
            Expr::Shell(shell) if shell.statements.is_empty() => {
                CompiledExpression::MixedShell(self.lower_shell(shell)?)
            }
            Expr::Shell(shell) => CompiledExpression::ShellProgram {
                body: self.lower_block(&shell.statements)?,
                span: shell.span.clone(),
            },
            Expr::ExecShell(shell) => CompiledExpression::ExecShell(self.lower_shell(shell)?),
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
                            ShellStep::MixedPipeline(pipeline) => CompiledShellStep::MixedPipeline(
                                Box::new(self.lower_mixed_shell_pipeline(pipeline)?),
                            ),
                        },
                    ))
                })
                .collect::<Result<Vec<_>, SparError>>()?,
            span: shell.span.clone(),
        })
    }

    fn lower_mixed_shell_pipeline(
        &mut self,
        pipeline: &crate::ast::ShellMixedPipeline,
    ) -> Result<CompiledShellMixedPipeline, SparError> {
        let input = pipeline
            .input
            .iter()
            .map(|command| self.lower_shell_command(command))
            .collect::<Result<Vec<_>, _>>()?;
        let output = pipeline
            .output
            .iter()
            .map(|command| self.lower_shell_command(command))
            .collect::<Result<Vec<_>, _>>()?;

        let mut current_type = mixed_decoder_stream_type(&pipeline.decoder);
        let mut stages = Vec::with_capacity(pipeline.stages.len());
        for (index, stage) in pipeline.stages.iter().enumerate() {
            self.locals.scopes.push(HashMap::new());
            let temp_name = format!("__sparMixedInput{index}");
            let input_slot = self
                .locals
                .allocate(temp_name.clone(), current_type.clone());
            let input_expr = Expr::NamespaceRef(crate::ast::NamespaceRef {
                segments: vec![temp_name],
                span: pipeline.decoder.span.clone(),
            });
            let expression = Expr::StructuredPipe {
                input: Box::new(input_expr),
                stage: Box::new(stage.clone()),
                span: stage
                    .span()
                    .cloned()
                    .unwrap_or_else(|| pipeline.span.clone()),
            };
            let next_type = self.locals.expression_type(&expression).ok_or_else(|| {
                internal_lowering(
                    "missing checked type for mixed structured pipeline stage",
                    &pipeline.span,
                )
            })?;
            let compiled = self.lower_expression(&expression);
            self.locals.scopes.pop();
            stages.push(CompiledShellStructuredStage {
                input_slot,
                expression: compiled?,
                span: stage
                    .span()
                    .cloned()
                    .unwrap_or_else(|| pipeline.span.clone()),
            });
            current_type = next_type;
        }

        let decoder = CompiledShellDecodeStage {
            namespace: pipeline.decoder.decoder.namespace,
            name: pipeline.decoder.decoder.name.clone(),
            args: pipeline
                .decoder
                .args
                .iter()
                .map(|arg| {
                    Ok(CompiledDecoderArg {
                        name: arg.name.clone(),
                        value: self.lower_expression(&arg.value)?,
                        span: arg.span.clone(),
                    })
                })
                .collect::<Result<Vec<_>, SparError>>()?,
            span: pipeline.decoder.span.clone(),
        };

        Ok(CompiledShellMixedPipeline {
            input,
            decoder,
            stages,
            encoder_format: pipeline
                .encoder
                .as_ref()
                .map(|encoder| encoder.format.clone()),
            encoder_span: pipeline.encoder.as_ref().map_or_else(
                || pipeline.decoder.span.clone(),
                |encoder| encoder.span.clone(),
            ),
            encoder_redirect: pipeline
                .encoder_redirect
                .as_ref()
                .map(|redirect| self.lower_shell_redirect(redirect))
                .transpose()?,
            output,
            span: pipeline.span.clone(),
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
                .map(|entry| Ok((entry.name.clone(), self.lower_shell_word(&entry.value)?)))
                .collect::<Result<Vec<_>, SparError>>()?,
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
            redirections: command
                .redirections
                .iter()
                .map(|redirect| {
                    Ok(crate::compiled::CompiledShellFdRedirect {
                        fd: redirect.fd,
                        target: match &redirect.target {
                            ShellFdRedirectTarget::File(file) => {
                                crate::compiled::CompiledShellFdRedirectTarget::File(
                                    self.lower_shell_redirect(file)?,
                                )
                            }
                            ShellFdRedirectTarget::Duplicate(fd) => {
                                crate::compiled::CompiledShellFdRedirectTarget::Duplicate(*fd)
                            }
                        },
                    })
                })
                .collect::<Result<Vec<_>, SparError>>()?,
            background: command.background,
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
                    ShellWordPart::CommandSubstitution(shell) => Ok(
                        CompiledShellWordPart::CommandSubstitution(self.lower_shell(shell)?),
                    ),
                })
                .collect::<Result<Vec<_>, SparError>>()?,
            span: word.span.clone(),
        })
    }

    fn lower_closure(
        &mut self,
        params: &[crate::ast::ClosureParam],
        explicit_return: Option<&SparType>,
        body: &crate::ast::ClosureBody,
        span: &Span,
        expected: Option<&SparType>,
    ) -> Result<CompiledExpression, SparError> {
        let expected_signature = match expected {
            Some(SparType::Function {
                params,
                return_type,
            }) => Some((params.as_slice(), return_type.as_ref())),
            _ => None,
        };
        let captures = self.locals.visible_bindings();
        let mut nested = FunctionLowerer {
            locals: LocalAllocator::new(self.locals.symbols),
            context: self.context,
            return_type: explicit_return
                .cloned()
                .or_else(|| expected_signature.map(|(_, ret)| ret.clone()))
                .unwrap_or(SparType::Void),
        };
        let mut compiled_captures = Vec::with_capacity(captures.len());
        for (name, source_slot, ty) in captures {
            let target_slot = nested.locals.allocate(name, ty);
            compiled_captures.push((
                target_slot,
                CompiledExpression::Local(source_slot, span.clone()),
            ));
        }
        let mut parameter_slots = Vec::with_capacity(params.len());
        for (index, param) in params.iter().enumerate() {
            let ty = param
                .ty
                .clone()
                .or_else(|| expected_signature.and_then(|(types, _)| types.get(index).cloned()))
                .ok_or_else(|| {
                    internal_lowering("missing checked closure parameter type", &param.span)
                })?;
            parameter_slots.push(nested.locals.allocate(param.name.clone(), ty));
        }
        let compiled_body = match body {
            crate::ast::ClosureBody::Expr(value) => {
                let expected_return = nested.return_type.clone();
                vec![CompiledStatement::Return(
                    Some(nested.lower_expression_expected(value, Some(&expected_return))?),
                    span.clone(),
                )]
            }
            crate::ast::ClosureBody::Block(body) => nested.lower_statements(&body.stmts)?,
        };
        Ok(CompiledExpression::Closure {
            captures: compiled_captures,
            parameter_slots,
            slot_count: nested.locals.names.len(),
            body: compiled_body,
            module: self.context.module,
            span: span.clone(),
        })
    }

    fn lower_call(
        &mut self,
        name: &str,
        arguments: &[crate::ast::CallArg],
        span: &Span,
    ) -> Result<CompiledExpression, SparError> {
        if !name.contains("::") {
            let path = vec![name.to_string()];
            if self
                .locals
                .symbols
                .lookup_section(&path)
                .is_some_and(|section| section.canonical)
            {
                return Ok(CompiledExpression::StructConstruct {
                    module: self.context.module,
                    name: name.to_string(),
                    overrides: arguments
                        .iter()
                        .map(|argument| {
                            Ok((
                                argument.param_name.clone(),
                                self.lower_expression(&argument.value)?,
                            ))
                        })
                        .collect::<Result<Vec<_>, SparError>>()?,
                    span: span.clone(),
                });
            }
        }
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
        if let [namespace, function_name] = segments.as_slice() {
            if let Some(signature) = self
                .locals
                .symbols
                .natives
                .get(&(namespace.to_string(), function_name.to_string()))
                .cloned()
            {
                let mut ordered = Vec::with_capacity(signature.params.len());
                for (parameter_name, _) in &signature.params {
                    let argument = arguments
                        .iter()
                        .find(|argument| &argument.param_name == parameter_name)
                        .ok_or_else(|| {
                            internal_lowering(
                                &format!(
                                    "native call '{}::{}' is missing checked argument '{}'",
                                    namespace, function_name, parameter_name
                                ),
                                span,
                            )
                        })?;
                    ordered.push(self.lower_expression(&argument.value)?);
                }
                return Ok(CompiledExpression::NativeCall {
                    function: signature.id,
                    arguments: ordered,
                    span: span.clone(),
                });
            }
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

fn is_untyped_closure(expression: &Expr) -> bool {
    matches!(expression, Expr::Closure { params, .. } if params.iter().any(|param| param.ty.is_none()))
}

/// Fills in untyped closure parameters from an inferred parameter list.
fn annotate_closure_parameters(expression: &mut Expr, types: &[SparType]) {
    if let Expr::Closure { params, .. } = expression {
        for (param, ty) in params.iter_mut().zip(types) {
            if param.ty.is_none() {
                param.ty = Some(ty.clone());
            }
        }
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
        | Expr::Closure { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Await { span, .. }
        | Expr::Comprehension { span, .. }
        | Expr::Index { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::StructuredPipe { span, .. }
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
