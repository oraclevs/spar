//! Keep selectively imported implementation details in their declaring scope.
use crate::ast::*;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

#[derive(Default)]
struct Remapping {
    identifiers: HashMap<String, String>,
    types: HashMap<String, SparType>,
}

pub(super) fn isolate(program: &mut Program, requested: &[ImportItem], path: &Path) {
    for item in &mut program.items {
        match item {
            TopLevelItem::Struct(decl) => {
                if decl.origin.is_none() {
                    decl.origin_private = decl.private;
                }
                decl.origin
                    .get_or_insert_with(|| (path.to_path_buf(), decl.name.clone()));
            }
            TopLevelItem::Enum(decl) => {
                decl.origin
                    .get_or_insert_with(|| (path.to_path_buf(), decl.name.clone()));
            }
            TopLevelItem::Impl(decl) => {
                decl.origin.get_or_insert_with(|| path.to_path_buf());
            }
            _ => {}
        }
    }
    let public: HashSet<_> = requested.iter().map(|item| item.name.as_str()).collect();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hash);
    let prefix = format!("sparModule{:x}", hash.finish());
    let names = program
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Var(decl) => Some(decl.name.clone()),
            TopLevelItem::Function(decl) => Some(decl.name.clone()),
            TopLevelItem::Struct(decl) => Some(decl.name.clone()),
            TopLevelItem::Enum(decl) => Some(decl.name.clone()),
            _ => None,
        })
        .filter(|name| !public.contains(name.as_str()))
        .map(|name| {
            let prefix = if name.starts_with(char::is_uppercase) {
                format!("S{}", &prefix[1..])
            } else {
                prefix.clone()
            };
            let replacement = format!("{prefix}{}{}", name[..1].to_uppercase(), &name[1..]);
            (name, replacement)
        })
        .collect::<HashMap<_, _>>();
    let names = Remapping {
        identifiers: names,
        ..Default::default()
    };
    for item in &mut program.items {
        visit_item(item, &names, &mut HashSet::new());
        match item {
            TopLevelItem::Var(decl) => rename(&mut decl.name, &names),
            TopLevelItem::Function(decl) => rename(&mut decl.name, &names),
            TopLevelItem::Struct(decl) => rename(&mut decl.name, &names),
            TopLevelItem::Enum(decl) => rename(&mut decl.name, &names),
            _ => {}
        }
    }
}

pub(super) fn dependencies(item: &TopLevelItem) -> HashSet<String> {
    let mut result = HashSet::new();
    visit_item(&mut item.clone(), &Remapping::default(), &mut result);
    result
}

pub(super) fn remap_references(item: &mut TopLevelItem, names: &HashMap<String, String>) {
    let mapping = Remapping {
        identifiers: names.clone(),
        ..Default::default()
    };
    visit_item(item, &mapping, &mut HashSet::new());
}

pub(crate) fn substitute_default(value: &Expr, substitution: &HashMap<String, SparType>) -> Expr {
    let mut value = value.clone();
    let mapping = Remapping {
        types: substitution.clone(),
        ..Default::default()
    };
    expr(&mut value, &HashSet::new(), &mapping, &mut HashSet::new());
    value
}

fn ty(value: &mut SparType, names: &Remapping, out: &mut HashSet<String>) {
    let mut refs = Vec::new();
    super::collect_spar_type_refs(value, &mut refs);
    out.extend(refs);
    *value = crate::typechecker::substitute_type(value, &names.types);
    for (from, to) in &names.identifiers {
        super::rename_spar_type(value, from, to);
    }
}

fn rename(name: &mut String, names: &Remapping) {
    if let Some(replacement) = names.identifiers.get(name) {
        *name = replacement.clone();
    }
}

fn reference(
    name: &mut String,
    locals: &HashSet<String>,
    names: &Remapping,
    out: &mut HashSet<String>,
) {
    if !locals.contains(name) {
        rename(name, names);
        out.insert(name.clone());
    }
}

fn function(function: &mut FunctionDecl, names: &Remapping, out: &mut HashSet<String>) {
    ty(&mut function.ret, names, out);
    let mut locals = HashSet::new();
    for parameter in &mut function.params {
        ty(&mut parameter.ty, names, out);
        if let Some(default) = &mut parameter.default {
            expr(default, &locals, names, out);
        }
        locals.insert(parameter.name.clone());
    }
    statements(&mut function.body.stmts, &locals, names, out);
}

fn visit_item(item: &mut TopLevelItem, names: &Remapping, out: &mut HashSet<String>) {
    let locals = HashSet::new();
    match item {
        TopLevelItem::Var(decl) => {
            ty(&mut decl.ty, names, out);
            if let Some(value) = &mut decl.value {
                expr(value, &locals, names, out);
            }
        }
        TopLevelItem::Struct(decl) => items(&mut decl.items, &locals, names, out),
        TopLevelItem::Function(decl) => function(decl, names, out),
        TopLevelItem::Impl(decl) => {
            ty(&mut decl.target, names, out);
            for method in &mut decl.methods {
                function(&mut method.function, names, out);
            }
        }
        TopLevelItem::FunctionGroup(group) => {
            for decl in &mut group.functions {
                function(decl, names, out);
            }
        }
        _ => {}
    }
}

fn statements(
    body: &mut [Statement],
    outer: &HashSet<String>,
    names: &Remapping,
    out: &mut HashSet<String>,
) {
    let mut locals = outer.clone();
    for statement in body {
        match statement {
            Statement::TupleBinding {
                names: bindings,
                ty: declared,
                value,
                ..
            } => {
                if let Some(declared) = declared {
                    ty(declared, names, out);
                }
                expr(value, &locals, names, out);
                for (name, _) in bindings {
                    locals.insert(name.clone());
                }
            }
            Statement::LocalVar(decl) => {
                if let Some(value) = &mut decl.ty {
                    ty(value, names, out);
                }
                expr(&mut decl.value, &locals, names, out);
                locals.insert(decl.name.clone());
            }
            Statement::Assignment { name, value, .. } => {
                reference(name, &locals, names, out);
                expr(value, &locals, names, out);
            }
            Statement::FieldAssignment { base, value, .. } => {
                reference(base, &locals, names, out);
                expr(value, &locals, names, out);
            }
            Statement::Expression(value, _) | Statement::Return(ReturnValue::Expr(value), _) => {
                expr(value, &locals, names, out)
            }
            Statement::If(branch) => {
                expr(&mut branch.condition, &locals, names, out);
                statements(&mut branch.then_stmts, &locals, names, out);
                statements(&mut branch.else_stmts, &locals, names, out);
            }
            Statement::For(looped) => {
                expr(&mut looped.iterable, &locals, names, out);
                let mut nested = locals.clone();
                match &looped.binding {
                    ForBinding::Value { name, .. } => {
                        nested.insert(name.clone());
                    }
                    ForBinding::Indexed {
                        index_name,
                        value_name,
                        ..
                    } => {
                        nested.insert(index_name.clone());
                        nested.insert(value_name.clone());
                    }
                }
                statements(&mut looped.body, &nested, names, out);
            }
            Statement::While(looped) => {
                if let Some(condition) = &mut looped.condition {
                    expr(condition, &locals, names, out);
                }
                statements(&mut looped.body, &locals, names, out);
            }
            Statement::Try(attempt) => {
                statements(&mut attempt.body, &locals, names, out);
                let mut nested = locals.clone();
                if let Some(name) = &attempt.catch_name {
                    nested.insert(name.clone());
                }
                statements(&mut attempt.handler, &nested, names, out);
            }
            Statement::Break(_)
            | Statement::Continue(_)
            | Statement::Return(ReturnValue::Void, _) => {}
        }
    }
}

fn items(
    body: &mut [ObjectItem],
    locals: &HashSet<String>,
    names: &Remapping,
    out: &mut HashSet<String>,
) {
    for item in body {
        match item {
            ObjectItem::Field(field) => {
                if let Some(value) = &mut field.ty {
                    ty(value, names, out);
                }
                match &mut field.value {
                    Some(FieldValue::Expr(value)) => expr(value, locals, names, out),
                    Some(FieldValue::Object(body)) => items(body, locals, names, out),
                    None => {}
                }
            }
            ObjectItem::Spread(spread) => expr(&mut spread.expr, locals, names, out),
        }
    }
}

fn expr(value: &mut Expr, locals: &HashSet<String>, names: &Remapping, out: &mut HashSet<String>) {
    match value {
        Expr::NamespaceRef(reference_value) => {
            if reference_value.segments.len() == 1 {
                reference(&mut reference_value.segments[0], locals, names, out);
            } else if reference_value.segments.len() == 2 && reference_value.segments[0] == "global"
            {
                reference(
                    &mut reference_value.segments[1],
                    &HashSet::new(),
                    names,
                    out,
                );
            } else if let Some(first) = reference_value.segments.first_mut() {
                reference(first, locals, names, out);
            }
        }
        Expr::Call {
            name,
            type_arguments,
            args,
            ..
        } => {
            reference(name, locals, names, out);
            for argument in type_arguments {
                ty(argument, names, out);
            }
            for arg in args {
                expr(&mut arg.value, locals, names, out);
            }
        }
        Expr::FnCall(call) => {
            reference(&mut call.name, locals, names, out);
            for arg in &mut call.args {
                expr(&mut arg.value, locals, names, out);
            }
        }
        Expr::Closure {
            params,
            return_type,
            body,
            ..
        } => {
            for parameter in params.iter_mut() {
                if let Some(value) = &mut parameter.ty {
                    ty(value, names, out);
                }
            }
            if let Some(value) = return_type {
                ty(value, names, out);
            }
            let mut nested = locals.clone();
            nested.extend(params.iter().map(|param| param.name.clone()));
            match body {
                ClosureBody::Expr(value) => expr(value, &nested, names, out),
                ClosureBody::Block(body) => statements(&mut body.stmts, &nested, names, out),
            }
        }
        Expr::BinaryOp(operation) => {
            expr(&mut operation.lhs, locals, names, out);
            expr(&mut operation.rhs, locals, names, out);
        }
        Expr::Unary { operand: value, .. }
        | Expr::Grouped(value, _)
        | Expr::TupleField { base: value, .. }
        | Expr::Await { value, .. } => expr(value, locals, names, out),
        Expr::FieldAccess { base, field, .. } => {
            if matches!(base.as_ref(), Expr::NamespaceRef(reference) if reference.segments == ["global"])
            {
                reference(field, &HashSet::new(), names, out);
            } else {
                expr(base, locals, names, out);
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            expr(receiver, locals, names, out);
            for arg in args {
                expr(&mut arg.value, locals, names, out);
            }
        }
        Expr::StructuredPipe { input, stage, .. } => {
            expr(input, locals, names, out);
            expr(stage, locals, names, out);
        }
        Expr::List(values, _) | Expr::Tuple(values, _) => {
            for value in values {
                expr(value, locals, names, out);
            }
        }
        Expr::Index { source, index, .. } => {
            expr(source, locals, names, out);
            expr(index, locals, names, out);
        }
        Expr::Comprehension {
            var_name,
            source,
            body,
            ..
        } => {
            expr(source, locals, names, out);
            let mut nested = locals.clone();
            nested.insert(var_name.clone());
            expr(body, &nested, names, out);
        }
        Expr::Object(body, _) => items(body, locals, names, out),
        Expr::String(string) => {
            for part in &mut string.parts {
                if let StringPart::Expr(value) = part {
                    expr(value, locals, names, out);
                }
            }
        }
        Expr::Shell(value) | Expr::ExecShell(value) | Expr::CommandSubstitution(value) => {
            shell(value, locals, names, out)
        }
        Expr::Literal(_) => {}
    }
}

fn shell(
    value: &mut ShellExpr,
    locals: &HashSet<String>,
    names: &Remapping,
    out: &mut HashSet<String>,
) {
    statements(&mut value.statements, locals, names, out);
    for (_, step) in &mut value.steps {
        match step {
            ShellStep::Command(value) => command(value, locals, names, out),
            ShellStep::Pipeline(values) => {
                for value in values {
                    command(value, locals, names, out);
                }
            }
            ShellStep::MixedPipeline(value) => {
                for command_value in value.input.iter_mut().chain(value.output.iter_mut()) {
                    command(command_value, locals, names, out);
                }
                for arg in &mut value.decoder.args {
                    expr(&mut arg.value, locals, names, out);
                }
                for stage in &mut value.stages {
                    expr(stage, locals, names, out);
                }
                if let Some(redirect) = &mut value.encoder_redirect {
                    word(&mut redirect.target, locals, names, out);
                }
            }
        }
    }
}

fn command(
    value: &mut ShellCommandExpr,
    locals: &HashSet<String>,
    names: &Remapping,
    out: &mut HashSet<String>,
) {
    word(&mut value.program, locals, names, out);
    for environment in &mut value.environment {
        word(&mut environment.value, locals, names, out);
    }
    for argument in &mut value.args {
        word(argument, locals, names, out);
    }
    for redirect in [&mut value.stdin, &mut value.stdout, &mut value.stderr]
        .into_iter()
        .flatten()
    {
        word(&mut redirect.target, locals, names, out);
    }
    for redirect in &mut value.redirections {
        if let ShellFdRedirectTarget::File(file) = &mut redirect.target {
            word(&mut file.target, locals, names, out);
        }
    }
}

fn word(
    value: &mut ShellWord,
    locals: &HashSet<String>,
    names: &Remapping,
    out: &mut HashSet<String>,
) {
    for part in &mut value.parts {
        match part {
            ShellWordPart::Expr(value) => expr(value, locals, names, out),
            ShellWordPart::CommandSubstitution(value) => shell(value, locals, names, out),
            _ => {}
        }
    }
}
