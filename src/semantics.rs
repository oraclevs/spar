//! Read-only semantic queries for editor tooling.
//!
//! The language server should use this boundary instead of duplicating Spar's
//! type-field substitution, callable, and method-signature rules.

use crate::ast::{SparType, TypeFieldShape};
use crate::resolver::{FunctionEntry, GlobalEntry, SymbolTable};
use crate::typechecker::{substitute_type, unify_generic, TypeChecker, TypeSubstitution};

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticField {
    pub name: String,
    pub ty: SparType,
    pub has_default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticParameter {
    pub name: String,
    pub ty: SparType,
    pub has_default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticCallable {
    pub name: String,
    pub parameters: Vec<SemanticParameter>,
    pub return_type: SparType,
    pub is_async: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticMethod {
    pub callable: SemanticCallable,
    pub receiver_mutable: bool,
    pub is_static: bool,
    pub is_native: bool,
}

#[derive(Debug, Clone)]
pub struct SemanticSnapshot {
    symbols: SymbolTable,
}

impl SemanticSnapshot {
    pub fn new(symbols: SymbolTable) -> Self {
        Self { symbols }
    }

    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    pub fn global_type(&self, name: &str) -> Option<SparType> {
        match self.symbols.globals.get(name)? {
            GlobalEntry::Var { ty, .. } => Some(ty.clone()),
            GlobalEntry::Dynamic { .. } => None,
        }
    }

    pub fn fields_for_type(&self, ty: &SparType) -> Vec<SemanticField> {
        TypeChecker::fields_for_type(ty, &self.symbols)
            .map(|(_, fields)| {
                fields
                    .into_iter()
                    .map(|field| SemanticField {
                        name: field.name,
                        ty: type_from_shape(&field.shape),
                        has_default: field.default.is_some(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn field_type(&self, ty: &SparType, field: &str) -> Option<SparType> {
        TypeChecker::field_type(ty, field, &self.symbols)
    }

    pub fn constructor(&self, ty: &SparType) -> Option<SemanticCallable> {
        let owner = owner_name_for_type(ty)?;
        self.symbols.structs.get(&vec![owner.to_string()])?;
        let (_, fields) = TypeChecker::fields_for_type(ty, &self.symbols)?;
        Some(SemanticCallable {
            name: owner.to_string(),
            parameters: fields.into_iter().map(|field| SemanticParameter {
                name: field.name, ty: type_from_shape(&field.shape), has_default: field.default.is_some(),
            }).collect(),
            return_type: ty.clone(), is_async: false,
        })
    }

    pub fn callable(&self, name: &str) -> Option<SemanticCallable> {
        if let Some(entry) = self
            .symbols
            .functions
            .get(name)
            .or_else(|| self.symbols.imported_functions.get(name))
        {
            return Some(callable_from_entry(name, entry, false));
        }
        let ty = self.global_type(name)?;
        let SparType::Function {
            params,
            return_type,
        } = ty
        else {
            return None;
        };
        Some(SemanticCallable {
            name: name.to_string(),
            parameters: params
                .into_iter()
                .map(|param| SemanticParameter {
                    name: param.name,
                    ty: param.ty,
                    has_default: false,
                })
                .collect(),
            return_type: *return_type,
            is_async: false,
        })
    }

    pub fn methods_for_type(&self, ty: &SparType) -> Vec<SemanticMethod> {
        if matches!(ty, SparType::Void) {
            return Vec::new();
        }

        let owner = owner_name_for_type(ty);
        let mut entries = Vec::new();
        if let Some(owner) = owner {
            if let Some(methods) = self.symbols.methods.get(owner) {
                entries.extend(methods.iter());
            }
        }
        if owner != Some("Any") {
            if let Some(universal) = self.symbols.methods.get("Any") {
                for (name, method) in universal {
                    if !entries
                        .iter()
                        .any(|(existing, _)| existing.as_str() == name.as_str())
                    {
                        entries.push((name, method));
                    }
                }
            }
        }

        let mut result = entries
            .into_iter()
            .filter(|(_, method)| !method.function.is_private)
            .map(|(name, method)| {
                let mut substitution = TypeSubstitution::new();
                if method.has_receiver {
                    if let Some((_, receiver_pattern)) = method.function.params.first() {
                        let _ = unify_generic(
                            receiver_pattern,
                            ty,
                            &mut substitution,
                            &crate::Span::dummy(),
                        );
                    }
                }
                let callable = SemanticCallable {
                    name: name.clone(),
                    parameters: method
                        .function
                        .params
                        .iter()
                        .skip(usize::from(method.has_receiver))
                        .map(|(param_name, param_ty)| SemanticParameter {
                            name: param_name.clone(),
                            ty: substitute_type(param_ty, &substitution),
                            has_default: method.function.default_params.contains(param_name),
                        })
                        .collect(),
                    return_type: substitute_type(&method.function.ret, &substitution),
                    is_async: method.function.is_async,
                };
                SemanticMethod {
                    callable,
                    receiver_mutable: method.receiver_mutable,
                    is_static: !method.has_receiver,
                    is_native: method.native_method.is_some(),
                }
            })
            .collect::<Vec<_>>();
        result.sort_by(|left, right| left.callable.name.cmp(&right.callable.name));
        result
    }
}

fn callable_from_entry(
    name: &str,
    entry: &FunctionEntry,
    skip_receiver: bool,
) -> SemanticCallable {
    SemanticCallable {
        name: name.to_string(),
        parameters: entry
            .params
            .iter()
            .skip(usize::from(skip_receiver))
            .map(|(param_name, ty)| SemanticParameter {
                name: param_name.clone(),
                ty: ty.clone(),
                has_default: entry.default_params.contains(param_name),
            })
            .collect(),
        return_type: entry.ret.clone(),
        is_async: entry.is_async,
    }
}

pub fn owner_name_for_type(ty: &SparType) -> Option<&str> {
    match ty {
        SparType::Named(name) | SparType::Applied { name, .. } => Some(name.as_str()),
        SparType::Str => Some("str"),
        SparType::Int => Some("int"),
        SparType::Float => Some("float"),
        SparType::Bool => Some("bool"),
        SparType::List(_) => Some("List"),
        SparType::InlineRecord => Some("Record"),
        _ => None,
    }
}

fn type_from_shape(shape: &TypeFieldShape) -> SparType {
    match shape {
        TypeFieldShape::Primitive(ty) => ty.clone(),
        TypeFieldShape::Named(name) => SparType::Named(name.clone()),
        TypeFieldShape::TypeParameter(name) => SparType::TypeParameter(name.clone()),
        TypeFieldShape::Applied { name, arguments } => SparType::Applied {
            name: name.clone(),
            arguments: arguments.clone(),
        },
        TypeFieldShape::InlineRecord(_) => SparType::InlineRecord,
    }
}
