use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::SparType;
use crate::error::{Span, SparError};

use super::context::RuntimeContext;
use super::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeFunctionId(pub(crate) u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeExecutionKind {
    Sync,
    Async,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIntrinsic {
    PromiseRace,
    PromiseTimeout,
}

pub type NativeCallback = Arc<
    dyn Fn(&mut RuntimeContext, &[Value]) -> Result<Value, SparError> + Send + Sync + 'static,
>;

#[derive(Clone)]
pub struct NativeFunction {
    pub module: String,
    pub name: String,
    pub params: Vec<(String, SparType)>,
    pub ret: SparType,
    pub execution: NativeExecutionKind,
    pub private: bool,
    pub(crate) handler: NativeHandler,
}

#[derive(Clone)]
pub(crate) enum NativeHandler {
    Callback(NativeCallback),
    Intrinsic(NativeIntrinsic),
}

impl NativeFunction {
    pub fn sync(
        module: impl Into<String>,
        name: impl Into<String>,
        params: Vec<(&str, SparType)>,
        ret: SparType,
        private: bool,
        callback: impl Fn(&mut RuntimeContext, &[Value]) -> Result<Value, SparError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            module: module.into(),
            name: name.into(),
            params: params
                .into_iter()
                .map(|(name, ty)| (name.to_string(), ty))
                .collect(),
            ret,
            execution: NativeExecutionKind::Sync,
            private,
            handler: NativeHandler::Callback(Arc::new(callback)),
        }
    }

    pub fn intrinsic(
        module: impl Into<String>,
        name: impl Into<String>,
        params: Vec<(&str, SparType)>,
        ret: SparType,
        private: bool,
        intrinsic: NativeIntrinsic,
    ) -> Self {
        Self {
            module: module.into(),
            name: name.into(),
            params: params
                .into_iter()
                .map(|(name, ty)| (name.to_string(), ty))
                .collect(),
            ret,
            execution: NativeExecutionKind::Async,
            private,
            handler: NativeHandler::Intrinsic(intrinsic),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeSignature {
    pub id: NativeFunctionId,
    pub params: Vec<(String, SparType)>,
    pub ret: SparType,
    pub execution: NativeExecutionKind,
    pub private: bool,
}

#[derive(Clone, Default)]
pub struct NativeRegistry {
    functions: Vec<NativeFunction>,
    by_name: HashMap<(String, String), NativeFunctionId>,
}

impl NativeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, function: NativeFunction) -> Result<NativeFunctionId, SparError> {
        let key = (function.module.clone(), function.name.clone());
        if self.by_name.contains_key(&key) {
            return Err(SparError::EvalError {
                message: format!(
                    "native function '{}::{}' is already registered",
                    key.0, key.1
                ),
                span: Span::dummy(),
            });
        }
        let id = NativeFunctionId(self.functions.len() as u32);
        self.functions.push(function);
        self.by_name.insert(key, id);
        Ok(id)
    }

    /// Registers every function of `other` that is not already present.
    /// Existing entries (and their ids) win, so an embedder's own natives
    /// are never shadowed by the ones merged in behind them.
    pub fn extend_missing(&mut self, other: &NativeRegistry) {
        for function in &other.functions {
            let key = (function.module.clone(), function.name.clone());
            if !self.by_name.contains_key(&key) {
                let _ = self.register(function.clone());
            }
        }
    }

    pub fn get(&self, module: &str, name: &str) -> Option<(NativeFunctionId, &NativeFunction)> {
        let id = *self.by_name.get(&(module.to_string(), name.to_string()))?;
        self.functions.get(id.0 as usize).map(|function| (id, function))
    }

    pub fn signature(&self, module: &str, name: &str) -> Option<NativeSignature> {
        let (id, function) = self.get(module, name)?;
        Some(NativeSignature {
            id,
            params: function.params.clone(),
            ret: function.ret.clone(),
            execution: function.execution,
            private: function.private,
        })
    }

    pub fn signatures(&self) -> HashMap<(String, String), NativeSignature> {
        self.by_name
            .iter()
            .filter_map(|(key, id)| {
                self.functions.get(id.0 as usize).map(|function| {
                    (
                        key.clone(),
                        NativeSignature {
                            id: *id,
                            params: function.params.clone(),
                            ret: function.ret.clone(),
                            execution: function.execution,
                            private: function.private,
                        },
                    )
                })
            })
            .collect()
    }

    pub fn intrinsic(&self, id: NativeFunctionId) -> Option<NativeIntrinsic> {
        let function = self.functions.get(id.0 as usize)?;
        match &function.handler {
            NativeHandler::Intrinsic(intrinsic) => Some(*intrinsic),
            NativeHandler::Callback(_) => None,
        }
    }

    pub fn call(
        &self,
        id: NativeFunctionId,
        context: &mut RuntimeContext,
        args: &[Value],
        span: &Span,
    ) -> Result<Value, SparError> {
        let function = self.functions.get(id.0 as usize).ok_or_else(|| SparError::EvalError {
            message: format!("unknown native function ID {}", id.0),
            span: span.clone(),
        })?;
        match &function.handler {
            NativeHandler::Callback(callback) => {
                callback(context, args).map_err(|error| with_call_span(error, span))
            }
            NativeHandler::Intrinsic(_) => Err(SparError::EvalError {
                message: format!(
                    "native runtime intrinsic '{}::{}' can only execute in the compiled runtime",
                    function.module, function.name
                ),
                span: span.clone(),
            }),
        }
    }
}

fn with_call_span(error: SparError, call_span: &Span) -> SparError {
    fn span_is_dummy(span: &Span) -> bool {
        span.start == 0 && span.end == 0 && span.line == 0 && span.col == 0
    }

    match error {
        SparError::LexError { message, span } if span_is_dummy(&span) => SparError::LexError {
            message,
            span: call_span.clone(),
        },
        SparError::ParseError { message, span } if span_is_dummy(&span) => SparError::ParseError {
            message,
            span: call_span.clone(),
        },
        SparError::ResolveError { message, hint, span } if span_is_dummy(&span) => {
            SparError::ResolveError {
                message,
                hint,
                span: call_span.clone(),
            }
        }
        SparError::TypeError { message, hint, span } if span_is_dummy(&span) => SparError::TypeError {
            message,
            hint,
            span: call_span.clone(),
        },
        SparError::EvalError { message, span } if span_is_dummy(&span) => SparError::EvalError {
            message,
            span: call_span.clone(),
        },
        SparError::SchemaError { message, span } if span_is_dummy(&span) => SparError::SchemaError {
            message,
            span: call_span.clone(),
        },
        other => other,
    }
}

impl std::fmt::Debug for NativeRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names = self.by_name.keys().cloned().collect::<Vec<_>>();
        names.sort();
        formatter
            .debug_struct("NativeRegistry")
            .field("functions", &names)
            .finish()
    }
}
