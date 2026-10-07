use std::collections::HashMap;

use crate::ast::{
    BinOp, ClosureBody, Expr, FieldValue, ForBinding, FunctionDecl, Literal, ObjectItem,
    ReturnValue, ShellCommandExpr, ShellExpr, ShellFdRedirectTarget, ShellRedirect, ShellStep,
    ShellWord, ShellWordPart, SparType, Statement, StringPart, UnOp,
};
use crate::compiled::{
    CompiledDecoderArg, CompiledExpression, CompiledLValue, CompiledMethodTarget,
    CompiledObjectItem, CompiledShellCommand, CompiledShellDecodeStage, CompiledShellExpr,
    CompiledShellMixedPipeline, CompiledShellRedirect, CompiledShellStep,
    CompiledShellStructuredStage, CompiledShellWord, CompiledShellWordPart, CompiledStatement,
    CompiledStringPart, FunctionId, FunctionKey, LocalLayout, LocalSlot, ModuleId, TypedOperation,
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
        SparType::Int => Some("int".into()),
        SparType::Float => Some("float".into()),
        SparType::Bool => Some("bool".into()),
        SparType::List(_) => Some("List".into()),
        SparType::InlineRecord => Some("Record".into()),
        SparType::Void => None,
        _ => Some("Any".into()),
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
    pub parameters: &'a HashMap<FunctionId, Vec<(String, SparType)>>,
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
        constructor_stack: Vec::new(),
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
                .map(|value| lowerer.lower_expression_expected(value, Some(&parameter.ty)))
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
    /// Values of local `const` bindings, keyed by slot so shadowing is exact.
    const_values: HashMap<LocalSlot, crate::ConfigValue>,
}

pub(crate) fn lower_default(
    expression: &Expr,
    expected: &SparType,
    symbols: &SymbolTable,
    context: LoweringContext<'_>,
) -> Result<(CompiledExpression, usize), SparError> {
    let mut lowerer = FunctionLowerer {
        constructor_stack: Vec::new(),
        locals: LocalAllocator::new(symbols),
        context,
        return_type: SparType::Void,
    };
    let expression = lowerer.lower_expression_expected(expression, Some(expected))?;
    Ok((expression, lowerer.locals.names.len()))
}

/// Whether a folded constant is representable as declared type `ty` without conversion.
fn value_fits(value: &crate::ConfigValue, ty: &SparType) -> bool {
    matches!(
        (value, ty),
        (crate::ConfigValue::Int(_), SparType::Int)
            | (crate::ConfigValue::Float(_), SparType::Float)
            | (crate::ConfigValue::Bool(_), SparType::Bool)
            | (crate::ConfigValue::Str(_), SparType::Str)
    )
}

impl<'a> LocalAllocator<'a> {
    fn new(symbols: &'a SymbolTable) -> Self {
        Self {
            symbols,
            scopes: vec![HashMap::new()],
            names: Vec::new(),
            types: Vec::new(),
            const_values: HashMap::new(),
        }
    }

    /// Constant value of bare name `name` as seen from the current scope: a local `const`
    /// (found through its slot), or a top-level `const` not shadowed by any local.
    fn const_lookup(&self, name: &str) -> Option<crate::ConfigValue> {
        match self.lookup_typed(name) {
            Some((slot, _)) => self.const_values.get(&slot).cloned(),
            None => {
                let value = self.symbols.constants.get(name)?;
                match self.symbols.globals.get(name) {
                    Some(crate::resolver::GlobalEntry::Var { ty, .. }) if value_fits(value, ty) => {
                        Some(value.clone())
                    }
                    _ => None,
                }
            }
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

    fn lookup_typed(&self, name: &str) -> Option<(LocalSlot, SparType)> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).map(|(slot, ty)| (*slot, ty.clone())))
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
                Statement::TupleBinding {
                    names, ty, value, ..
                } => {
                    self.visit_expression(value);
                    if let Some(SparType::Tuple(items)) =
                        ty.clone().or_else(|| self.expression_type(value))
                    {
                        for ((name, _), ty) in names.iter().zip(items) {
                            self.allocate(name.clone(), ty);
                        }
                    }
                }
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
                Statement::While(statement) => {
                    if let Some(condition) = &statement.condition {
                        self.visit_expression(condition);
                    }
                    self.with_scope(|this| this.visit_statements(&statement.body));
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
                    self.visit_expression(&argument.value);
                }
            }
            Expr::BinaryOp(operation) => {
                self.visit_expression(&operation.lhs);
                self.visit_expression(&operation.rhs);
            }
            Expr::List(items, _) | Expr::Tuple(items, _) => {
                for item in items {
                    self.visit_expression(item);
                }
            }
            Expr::Grouped(inner, _)
            | Expr::TupleField { base: inner, .. }
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
                    self.visit_expression(&argument.value);
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
                        ObjectItem::Field(field) => match &field.value {
                            Some(FieldValue::Expr(value)) => self.visit_expression(value),
                            Some(FieldValue::Object(_)) | None => {}
                        },
                        ObjectItem::Spread(spread) => self.visit_expression(&spread.expr),
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
    constructor_stack: Vec<String>,
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
            Statement::TupleBinding {
                names,
                ty,
                value,
                span,
            } => {
                let tuple_ty = ty
                    .clone()
                    .or_else(|| self.locals.expression_type(value))
                    .ok_or_else(|| internal_lowering("missing checked tuple binding type", span))?;
                let SparType::Tuple(items) = &tuple_ty else {
                    return Err(internal_lowering("tuple binding source is not tuple", span));
                };
                let compiled_value = self.lower_expression_expected(value, Some(&tuple_ty))?;
                let slots = names
                    .iter()
                    .zip(items)
                    .map(|((name, _), ty)| self.locals.allocate(name.clone(), ty.clone()))
                    .collect();
                CompiledStatement::TupleBinding {
                    slots,
                    value: compiled_value,
                    span: span.clone(),
                }
            }
            Statement::LocalVar(local) => {
                let ty = local
                    .ty
                    .clone()
                    .or_else(|| self.locals.expression_type(&local.value))
                    .ok_or_else(|| internal_lowering("missing checked local type", &local.span))?;
                let value = self.lower_expression_expected(&local.value, Some(&ty))?;
                // Evaluate a `const` before its own slot exists so it cannot see itself.
                let folded = if local.is_const {
                    crate::constants::eval(
                        &local.value,
                        &|name| self.locals.const_lookup(name),
                        &local.span,
                    )
                    .ok()
                    .filter(|value| value_fits(value, &ty))
                } else {
                    None
                };
                let slot = self.locals.allocate(local.name.clone(), ty);
                let value = match folded {
                    Some(folded) => {
                        self.locals.const_values.insert(slot, folded.clone());
                        CompiledExpression::Constant(folded, local.span.clone())
                    }
                    None => value,
                };
                CompiledStatement::StoreLocal {
                    slot,
                    value,
                    span: local.span.clone(),
                }
            }
            Statement::Assignment { name, value, span } => {
                if let Some((slot, ty)) = self.locals.lookup_typed(name) {
                    CompiledStatement::StoreLocal {
                        slot,
                        value: self.lower_expression_expected(value, Some(&ty))?,
                        span: span.clone(),
                    }
                } else {
                    let expected =
                        self.locals
                            .symbols
                            .globals
                            .get(name)
                            .and_then(|entry| match entry {
                                crate::resolver::GlobalEntry::Var { ty, .. } => Some(ty.clone()),
                                crate::resolver::GlobalEntry::Dynamic { .. } => None,
                            });
                    CompiledStatement::StoreGlobal {
                        name: name.clone(),
                        value: self.lower_expression_expected(value, expected.as_ref())?,
                        span: span.clone(),
                    }
                }
            }
            Statement::FieldAssignment {
                base,
                fields,
                value,
                span,
            } => {
                let mut expected = self
                    .locals
                    .lookup_typed(base)
                    .map(|(_, ty)| ty)
                    .or_else(|| {
                        self.locals
                            .symbols
                            .globals
                            .get(base)
                            .and_then(|entry| match entry {
                                crate::resolver::GlobalEntry::Var { ty, .. } => Some(ty.clone()),
                                crate::resolver::GlobalEntry::Dynamic { .. } => None,
                            })
                    });
                for field in fields {
                    expected = expected.and_then(|owner| {
                        crate::typechecker::TypeChecker::field_type(
                            &owner,
                            field,
                            self.locals.symbols,
                        )
                    });
                }
                let value = self.lower_expression_expected(value, expected.as_ref())?;
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
            Statement::While(statement) => {
                let condition = match &statement.condition {
                    Some(condition) => Some(self.lower_expression(condition)?),
                    None => None,
                };
                self.locals.scopes.push(HashMap::new());
                let body = self.lower_statements(&statement.body);
                self.locals.scopes.pop();
                CompiledStatement::While {
                    condition,
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
                        match self.locals.const_values.get(&slot) {
                            Some(value) => {
                                CompiledExpression::Constant(value.clone(), reference.span.clone())
                            }
                            None => CompiledExpression::Local(slot, reference.span.clone()),
                        }
                    } else if let Some(value) = self.locals.const_lookup(&reference.segments[0]) {
                        CompiledExpression::Constant(value, reference.span.clone())
                    } else if self
                        .locals
                        .symbols
                        .lookup_struct(&reference.segments)
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
                } else if reference.segments.len() == 2
                    && self
                        .locals
                        .symbols
                        .enums
                        .contains_key(&reference.segments[0])
                {
                    CompiledExpression::Constant(
                        crate::ConfigValue::Str(reference.segments[1].clone()),
                        reference.span.clone(),
                    )
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
            } => {
                let return_type = self
                    .locals
                    .expression_type(expression)
                    .or_else(|| expected.cloned());
                self.lower_call(name, args, span, return_type)?
            }
            Expr::FnCall(call) => {
                let return_type = self
                    .locals
                    .expression_type(expression)
                    .or_else(|| expected.cloned());
                self.lower_call(&call.name, &call.args, &call.span, return_type)?
            }
            Expr::StructuredPipe { input, stage, span } => {
                let input_value = self.lower_expression(input)?;
                match stage.as_ref() {
                    Expr::FnCall(call) => {
                        let mut arguments = Vec::with_capacity(call.args.len() + 1);
                        arguments.push(input_value);
                        // Closures with untyped parameters get them from the
                        // stage signature instantiated for the piped input.
                        let inferred = if call
                            .args
                            .iter()
                            .any(|argument| is_untyped_closure(&argument.value))
                        {
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
                            arguments
                                .push(self.lower_expression_expected(&argument.value, expected)?);
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
                        let visible_types = self.locals.visible_types();
                        let input_type = self.locals.expression_type(input).ok_or_else(|| {
                            internal_lowering("structured pipe input has no checked type", span)
                        })?;
                        let parameters = if let Some(parameters) =
                            crate::typechecker::pipe_stage_parameters_with_locals(
                                input,
                                stage,
                                self.locals.symbols,
                                &visible_types,
                            ) {
                            parameters
                        } else if let Some((_, SparType::Function { params, .. })) =
                            self.locals.lookup_typed(name)
                        {
                            params
                                .into_iter()
                                .map(|parameter| (parameter.name, parameter.ty))
                                .collect()
                        } else {
                            return Err(internal_lowering(
                                "structured pipe named stage has no checked callable signature",
                                span,
                            ));
                        };
                        let supplied = args
                            .iter()
                            .map(|argument| argument.param_name.as_str())
                            .collect::<std::collections::HashSet<_>>();
                        let implicit = parameters
                            .iter()
                            .find(|(parameter_name, parameter_type)| {
                                !supplied.contains(parameter_name.as_str())
                                    && crate::typechecker::pipe_type_accepts(
                                        parameter_type,
                                        &input_type,
                                    )
                            })
                            .ok_or_else(|| {
                                internal_lowering(
                                    "structured pipe has no checked compatible implicit parameter",
                                    span,
                                )
                            })?;
                        let mut injected = Vec::with_capacity(args.len() + 1);
                        injected.push(crate::ast::CallArg {
                            param_name: implicit.0.clone(),
                            param_name_span: span.clone(),
                            value: input.as_ref().clone(),
                            span: span.clone(),
                        });
                        injected.extend(args.iter().cloned());
                        if args.iter().any(|arg| is_untyped_closure(&arg.value)) {
                            for arg in injected.iter_mut() {
                                if is_untyped_closure(&arg.value) {
                                    if let Some((_, SparType::Function { params, .. })) = parameters
                                        .iter()
                                        .find(|(parameter, _)| parameter == &arg.param_name)
                                    {
                                        annotate_closure_parameters(&mut arg.value, params);
                                    }
                                }
                            }
                        }
                        let return_type = self.locals.expression_type(stage);
                        self.lower_call(name, &injected, call_span, return_type)?
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
                let structural_equality = matches!(binary.op, BinOp::Eq | BinOp::NotEq)
                    && !matches!(left_type, SparType::Int | SparType::Float | SparType::Str | SparType::Bool)
                    && !matches!(&left_type, SparType::Named(name) if self.locals.symbols.enums.contains_key(name));
                CompiledExpression::Operation {
                    operation: if dynamic_equality || structural_equality {
                        match binary.op {
                            BinOp::Eq => TypedOperation::DynamicEq,
                            BinOp::NotEq => TypedOperation::DynamicNotEq,
                            BinOp::Lt => TypedOperation::DynamicLt,
                            BinOp::Gt => TypedOperation::DynamicGt,
                            BinOp::LtEq => TypedOperation::DynamicLtEq,
                            _ => TypedOperation::DynamicGtEq,
                        }
                    } else if matches!(&left_type, SparType::Named(name) if self.locals.symbols.enums.contains_key(name))
                        && matches!(binary.op, BinOp::Eq | BinOp::NotEq)
                    {
                        if matches!(binary.op, BinOp::Eq) {
                            TypedOperation::StringEq
                        } else {
                            TypedOperation::StringNotEq
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
            Expr::List(items, span) => {
                let element_type = match expected {
                    Some(SparType::List(element)) => Some(element.as_ref()),
                    _ => None,
                };
                CompiledExpression::List(
                    items
                        .iter()
                        .map(|item| self.lower_expression_expected(item, element_type))
                        .collect::<Result<Vec<_>, _>>()?,
                    span.clone(),
                )
            }
            Expr::Tuple(items, span) => {
                let expected_items = match expected {
                    Some(SparType::Tuple(items)) => Some(items),
                    _ => None,
                };
                CompiledExpression::List(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| {
                            self.lower_expression_expected(
                                item,
                                expected_items.and_then(|items| items.get(index)),
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    span.clone(),
                )
            }
            Expr::Grouped(inner, _) => self.lower_expression_expected(inner, expected)?,
            Expr::TupleField {
                base, index, span, ..
            } => CompiledExpression::Index {
                source: Box::new(self.lower_expression(base)?),
                index: Box::new(CompiledExpression::Constant(
                    crate::ConfigValue::Int(*index as i64),
                    span.clone(),
                )),
                span: span.clone(),
            },
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
                            .lookup_struct(&reference.segments)
                            .is_some()
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
                let mut arguments = Vec::with_capacity(parameter_types.len());
                for (parameter_name, expected) in parameter_types {
                    if let Some(argument) = args
                        .iter()
                        .find(|argument| &argument.param_name == parameter_name)
                    {
                        let expected = substitute_type(expected, &substitution);
                        arguments.push(
                            self.lower_expression_expected(&argument.value, Some(&expected))?,
                        );
                    } else {
                        arguments.push(CompiledExpression::DefaultArgument(span.clone()));
                    }
                }
                let (receiver_expr, receiver_lvalue) = if entry.has_receiver {
                    let lvalue = if entry.receiver_mutable {
                        Some(self.mutable_receiver_target(receiver).ok_or_else(|| {
                            internal_lowering(
                                "mutable method receiver must be a mutable binding or field path",
                                span,
                            )
                        })?)
                    } else {
                        None
                    };
                    (Some(Box::new(self.lower_expression(receiver)?)), lvalue)
                } else {
                    (None, None)
                };
                CompiledExpression::MethodCall {
                    target,
                    receiver: receiver_expr,
                    receiver_lvalue,
                    arguments,
                    mutates_receiver: entry.receiver_mutable,
                    return_type: self.locals.expression_type(expression).or_else(|| expected.cloned()),
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
                let body_expected = match expected {
                    Some(SparType::List(element)) => Some(element.as_ref()),
                    _ => None,
                };
                let body = self.lower_expression_expected(body, body_expected);
                self.locals.scopes.pop();
                CompiledExpression::Comprehension {
                    binding,
                    source,
                    body: Box::new(body?),
                    span: span.clone(),
                }
            }
            Expr::Object(items, span) => match expected {
                Some(SparType::Applied { name, arguments })
                    if name == "Map" && arguments.len() == 2 =>
                {
                    CompiledExpression::Map(
                        self.lower_object_items(items, Some(&arguments[1]))?,
                        span.clone(),
                    )
                }
                _ => {
                    CompiledExpression::Object(self.lower_object_items(items, None)?, span.clone())
                }
            },
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
            run_now: shell.run_now,
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
            glob: word.glob,
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
            constructor_stack: self.constructor_stack.clone(),
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
                .or_else(|| {
                    expected_signature
                        .and_then(|(types, _)| types.get(index).map(|param| param.ty.clone()))
                })
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

    fn mutable_receiver_target(&self, receiver: &Expr) -> Option<CompiledLValue> {
        match receiver {
            Expr::NamespaceRef(reference) if reference.segments.len() == 1 => {
                let name = &reference.segments[0];
                if let Some(slot) = self.locals.lookup(name) {
                    Some(CompiledLValue::Local(slot))
                } else {
                    Some(CompiledLValue::Global(name.clone()))
                }
            }
            Expr::FieldAccess { base, field, .. } => Some(CompiledLValue::Field {
                base: Box::new(self.mutable_receiver_target(base)?),
                field: field.clone(),
            }),
            _ => None,
        }
    }

    fn lower_call(
        &mut self,
        name: &str,
        arguments: &[crate::ast::CallArg],
        span: &Span,
        return_type: Option<SparType>,
    ) -> Result<CompiledExpression, SparError> {
        if !name.contains("::") {
            if let Some((slot, SparType::Function { params, .. })) = self.locals.lookup_typed(name)
            {
                let mut ordered = Vec::with_capacity(params.len());
                for parameter in &params {
                    let argument = arguments
                        .iter()
                        .find(|argument| argument.param_name == parameter.name)
                        .ok_or_else(|| {
                            internal_lowering(
                                &format!(
                                    "callable '{name}' is missing checked argument '{}'",
                                    parameter.name
                                ),
                                span,
                            )
                        })?;
                    ordered.push(
                        self.lower_expression_expected(&argument.value, Some(&parameter.ty))?,
                    );
                }
                return Ok(CompiledExpression::Invoke {
                    callee: Box::new(CompiledExpression::Local(slot, span.clone())),
                    arguments: ordered,
                    span: span.clone(),
                });
            }
        }
        if !name.contains("::") {
            let path = vec![name.to_string()];
            if self.locals.symbols.lookup_struct(&path).is_some() {
                let owner = return_type
                    .clone()
                    .unwrap_or_else(|| SparType::Named(name.to_string()));
                let (_, fields) =
                    crate::typechecker::TypeChecker::fields_for_type(&owner, self.locals.symbols)
                        .ok_or_else(|| internal_lowering("missing struct fields", span))?;
                let mut values = Vec::new();
                for field in fields {
                    let expected = crate::typechecker::TypeChecker::field_type(
                        &owner,
                        &field.name,
                        self.locals.symbols,
                    );
                    let value = if let Some(argument) = arguments
                        .iter()
                        .find(|argument| argument.param_name == field.name)
                    {
                        self.lower_expression_expected(&argument.value, expected.as_ref())?
                    } else if let Some(default) = &field.default {
                        // Defaults resolve in module scope, never the constructor caller's locals.
                        if self.constructor_stack.iter().any(|owner| owner == name) {
                            return Err(internal_lowering(
                                "recursive struct defaults require an explicit optional boundary",
                                span,
                            ));
                        }
                        let mut stack = self.constructor_stack.clone();
                        stack.push(name.to_string());
                        let mut defaults = FunctionLowerer {
                            constructor_stack: stack,
                            locals: LocalAllocator::new(self.locals.symbols),
                            context: self.context,
                            return_type: SparType::Void,
                        };
                        // Share the frame's allocation range, but not its lexical names.
                        defaults.locals.names = self.locals.names.clone();
                        defaults.locals.types = self.locals.types.clone();
                        let expression =
                            defaults.lower_expression_expected(default, expected.as_ref())?;
                        self.locals.names = defaults.locals.names;
                        self.locals.types = defaults.locals.types;
                        expression
                    } else {
                        return Err(internal_lowering(
                            "missing required constructor argument",
                            span,
                        ));
                    };
                    values.push(CompiledObjectItem::Field {
                        name: field.name,
                        value,
                    });
                }
                return Ok(CompiledExpression::Object(values, span.clone()));
            }
        }
        if matches!(name, "str" | "int" | "float" | "bool") {
            let argument = arguments
                .iter()
                .find(|argument| argument.param_name == "value")
                .ok_or_else(|| internal_lowering("conversion value argument is unavailable", span))?;
            return Ok(CompiledExpression::Convert {
                kind: name.to_string(),
                value: Box::new(self.lower_expression(&argument.value)?),
                span: span.clone(),
            });
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
            let parameters =
                self.context.parameters.get(&function).ok_or_else(|| {
                    internal_lowering("direct call target has no signature", span)
                })?;
            let mut ordered = Vec::with_capacity(parameters.len());
            for (parameter_name, parameter_type) in parameters {
                if let Some(argument) = arguments
                    .iter()
                    .find(|argument| &argument.param_name == parameter_name)
                {
                    ordered.push(
                        self.lower_expression_expected(&argument.value, Some(parameter_type))?,
                    );
                } else {
                    ordered.push(CompiledExpression::DefaultArgument(span.clone()));
                }
            }
            return Ok(CompiledExpression::DirectCall {
                function,
                arguments: ordered,
                return_type: return_type.clone(),
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
                for (parameter_name, parameter_type) in &signature.params {
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
                    ordered.push(
                        self.lower_expression_expected(&argument.value, Some(parameter_type))?,
                    );
                }
                return Ok(CompiledExpression::NativeCall {
                    function: signature.id,
                    arguments: ordered,
                    return_type,
                    span: span.clone(),
                });
            }
        }
        let (namespace, function_name) = segments.split_at(segments.len().saturating_sub(1));
        let host_namespace = namespace.join("::");
        let host_name = function_name.first().copied().unwrap_or(name).to_string();
        let host_signature = self
            .locals
            .symbols
            .hosts
            .get(&(host_namespace.clone(), host_name.clone()))
            .cloned();
        let lowered_arguments = if let Some(signature) = host_signature {
            let mut lowered = Vec::with_capacity(signature.params.len());
            for (parameter_name, parameter_type) in &signature.params {
                let argument = arguments
                    .iter()
                    .find(|argument| &argument.param_name == parameter_name)
                    .ok_or_else(|| {
                        internal_lowering(
                            &format!(
                                "host call '{}::{}' is missing checked argument '{}'",
                                host_namespace, host_name, parameter_name
                            ),
                            span,
                        )
                    })?;
                lowered
                    .push(self.lower_expression_expected(&argument.value, Some(parameter_type))?);
            }
            lowered
        } else {
            arguments
                .iter()
                .map(|argument| self.lower_expression(&argument.value))
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(CompiledExpression::HostCall {
            namespace: host_namespace,
            name: host_name,
            arguments: lowered_arguments,
            span: span.clone(),
        })
    }

    fn lower_object_items(
        &mut self,
        items: &[ObjectItem],
        value_type: Option<&SparType>,
    ) -> Result<Vec<CompiledObjectItem>, SparError> {
        items
            .iter()
            .map(|item| match item {
                ObjectItem::Spread(spread) => Ok(CompiledObjectItem::Spread(
                    self.lower_expression(&spread.expr)?,
                )),
                ObjectItem::Field(field) => {
                    let value = match &field.value {
                        Some(FieldValue::Expr(value)) => {
                            self.lower_expression_expected(value, value_type)?
                        }
                        Some(FieldValue::Object(items)) => match value_type {
                            Some(SparType::Applied { name, arguments })
                                if name == "Map" && arguments.len() == 2 =>
                            {
                                CompiledExpression::Map(
                                    self.lower_object_items(items, Some(&arguments[1]))?,
                                    field.span.clone(),
                                )
                            }
                            _ => CompiledExpression::Object(
                                self.lower_object_items(items, None)?,
                                field.span.clone(),
                            ),
                        },
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
fn annotate_closure_parameters(expression: &mut Expr, types: &[crate::ast::CallableParamType]) {
    if let Expr::Closure { params, .. } = expression {
        for (param, ty) in params.iter_mut().zip(types) {
            if param.ty.is_none() {
                param.ty = Some(ty.ty.clone());
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
        (BinOp::Rem, SparType::Int) => TypedOperation::IntRem,
        (BinOp::Rem, SparType::Float) => TypedOperation::FloatRem,
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
        | Expr::Tuple(_, span)
        | Expr::TupleField { span, .. }
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
