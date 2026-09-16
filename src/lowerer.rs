use std::collections::HashMap;

use crate::ast::{
    Expr, FieldValue, ForBinding, FunctionDecl, ReturnValue, SectionItem, SparType, Statement,
    StringPart,
};
use crate::compiled::{LocalLayout, LocalSlot};
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
        self.scopes.last_mut().unwrap().insert(name, (slot, ty));
        slot
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
            Expr::Grouped(inner, _) | Expr::Unary { operand: inner, .. } => {
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
            Expr::Literal(_) | Expr::NamespaceRef(_) | Expr::Shell(_) | Expr::ExecShell(_) => {}
        }
    }
}
