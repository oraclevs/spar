#![allow(dead_code)] // Activated when compiled execution is installed in Task 5.

use std::collections::HashMap;

use crate::compiled::{
    CompiledExpression, CompiledObjectItem, CompiledProgram, CompiledStatement, CompiledStringPart,
    FunctionId, LocalSlot, TypedOperation,
};
use crate::error::{Span, SparError};
use crate::evaluator::ConfigValue;

pub(crate) struct Frame {
    slots: Vec<Option<ConfigValue>>,
}

impl Frame {
    pub fn new(slot_count: usize) -> Self {
        Self {
            slots: vec![None; slot_count],
        }
    }

    pub fn read(&self, slot: LocalSlot, span: &Span) -> Result<&ConfigValue, SparError> {
        self.slots
            .get(slot.0 as usize)
            .ok_or_else(|| internal_slot_error(slot, "is invalid", span))?
            .as_ref()
            .ok_or_else(|| internal_slot_error(slot, "is uninitialized", span))
    }

    pub fn write(
        &mut self,
        slot: LocalSlot,
        value: ConfigValue,
        span: &Span,
    ) -> Result<(), SparError> {
        let destination = self
            .slots
            .get_mut(slot.0 as usize)
            .ok_or_else(|| internal_slot_error(slot, "is invalid", span))?;
        *destination = Some(value);
        Ok(())
    }
}

fn internal_slot_error(slot: LocalSlot, detail: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("internal runtime error: local slot {} {detail}", slot.0),
        span: span.clone(),
    }
}

const MAX_CALL_DEPTH: usize = 20;

pub(crate) fn execute_self_contained_entry(
    program: &CompiledProgram,
) -> Result<ConfigValue, Vec<SparError>> {
    let entry = program.entry_main.ok_or_else(|| {
        vec![SparError::ResolveError {
            message: "no 'main' function found".into(),
            hint: None,
            span: Span::dummy(),
        }]
    })?;
    Runtime {
        program,
        call_depth: 0,
    }
    .call_function(entry, Vec::new())
    .map_err(|error| vec![error])
}

pub(crate) struct Runtime<'a> {
    program: &'a CompiledProgram,
    call_depth: usize,
}

enum RuntimeFlow {
    Normal,
    Break,
    Continue,
    Return(ConfigValue),
}

impl Runtime<'_> {
    fn call_function(
        &mut self,
        id: FunctionId,
        arguments: Vec<ConfigValue>,
    ) -> Result<ConfigValue, SparError> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(runtime_error(
                "maximum function call depth exceeded",
                &Span::dummy(),
            ));
        }
        let function = self
            .program
            .modules
            .iter()
            .flat_map(|module| module.functions.iter())
            .find(|function| function.id == id)
            .ok_or_else(|| {
                runtime_error(&format!("unknown function ID {}", id.0), &Span::dummy())
            })?;
        let parameter_slots = function.parameter_slots.clone();
        let default_values = function.default_values.clone();
        let slot_count = function.slot_count;
        let body = function.body.clone();
        let function_span = function.span.clone();
        if arguments.len() > parameter_slots.len() {
            return Err(runtime_error(
                "too many direct-call arguments",
                &function_span,
            ));
        }
        self.call_depth += 1;
        let result = (|| {
            let mut frame = Frame::new(slot_count);
            let supplied_count = arguments.len();
            for (slot, value) in parameter_slots.iter().copied().zip(arguments) {
                frame.write(slot, value, &function_span)?;
            }
            for (index, slot) in parameter_slots
                .iter()
                .copied()
                .enumerate()
                .skip(supplied_count)
            {
                let default = default_values
                    .get(index)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| {
                        runtime_error("missing required direct-call argument", &function_span)
                    })?;
                let value = self.eval_expression(default, &mut frame)?;
                frame.write(slot, value, &function_span)?;
            }
            match self.execute_statements(&body, &mut frame)? {
                RuntimeFlow::Return(value) => Ok(value),
                RuntimeFlow::Normal => Ok(ConfigValue::Int(0)),
                RuntimeFlow::Break | RuntimeFlow::Continue => Err(runtime_error(
                    "loop control escaped a compiled function",
                    &function_span,
                )),
            }
        })();
        self.call_depth -= 1;
        result
    }

    fn execute_statements(
        &mut self,
        statements: &[CompiledStatement],
        frame: &mut Frame,
    ) -> Result<RuntimeFlow, SparError> {
        for statement in statements {
            let flow = match statement {
                CompiledStatement::StoreLocal { slot, value, span } => {
                    let value = self.eval_expression(value, frame)?;
                    frame.write(*slot, value, span)?;
                    RuntimeFlow::Normal
                }
                CompiledStatement::StoreGlobal { span, .. } => {
                    return Err(module_state_error(span));
                }
                CompiledStatement::Expression(expression, _) => {
                    self.eval_expression(expression, frame)?;
                    RuntimeFlow::Normal
                }
                CompiledStatement::If {
                    condition,
                    then_body,
                    else_body,
                    span,
                } => match self.eval_expression(condition, frame)? {
                    ConfigValue::Bool(true) => self.execute_statements(then_body, frame)?,
                    ConfigValue::Bool(false) => self.execute_statements(else_body, frame)?,
                    value => return Err(type_error("bool", &value, span)),
                },
                CompiledStatement::For {
                    index_slot,
                    value_slot,
                    iterable,
                    body,
                    span,
                } => {
                    let ConfigValue::List(items) = self.eval_expression(iterable, frame)? else {
                        return Err(runtime_error("checked loop received a non-list", span));
                    };
                    let mut loop_flow = RuntimeFlow::Normal;
                    for (index, value) in items.into_iter().enumerate() {
                        if let Some(index_slot) = index_slot {
                            frame.write(*index_slot, ConfigValue::Int(index as i64), span)?;
                        }
                        frame.write(*value_slot, value, span)?;
                        match self.execute_statements(body, frame)? {
                            RuntimeFlow::Normal | RuntimeFlow::Continue => {}
                            RuntimeFlow::Break => break,
                            flow @ RuntimeFlow::Return(_) => {
                                loop_flow = flow;
                                break;
                            }
                        }
                    }
                    loop_flow
                }
                CompiledStatement::Return(value, _) => RuntimeFlow::Return(match value {
                    Some(value) => self.eval_expression(value, frame)?,
                    None => ConfigValue::Int(0),
                }),
                CompiledStatement::Break(_) => RuntimeFlow::Break,
                CompiledStatement::Continue(_) => RuntimeFlow::Continue,
            };
            if !matches!(flow, RuntimeFlow::Normal) {
                return Ok(flow);
            }
        }
        Ok(RuntimeFlow::Normal)
    }

    fn eval_expression(
        &mut self,
        expression: &CompiledExpression,
        frame: &mut Frame,
    ) -> Result<ConfigValue, SparError> {
        match expression {
            CompiledExpression::Constant(value, _) => Ok(value.clone()),
            CompiledExpression::Local(slot, span) => Ok(frame.read(*slot, span)?.clone()),
            CompiledExpression::Global(_, span)
            | CompiledExpression::ImportedValue { span, .. }
            | CompiledExpression::HostCall { span, .. }
            | CompiledExpression::ExecShell(crate::ast::ShellExpr { span, .. }) => {
                Err(module_state_error(span))
            }
            CompiledExpression::DirectCall {
                function,
                arguments,
                ..
            } => {
                let values = arguments
                    .iter()
                    .map(|argument| self.eval_expression(argument, frame))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call_function(*function, values)
            }
            CompiledExpression::List(items, _) => Ok(ConfigValue::List(
                items
                    .iter()
                    .map(|item| self.eval_expression(item, frame))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            CompiledExpression::Object(items, span) => {
                let mut object = HashMap::new();
                for item in items {
                    match item {
                        CompiledObjectItem::Field { name, value } => {
                            object.insert(name.clone(), self.eval_expression(value, frame)?);
                        }
                        CompiledObjectItem::Spread(value) => {
                            let ConfigValue::Section(fields) =
                                self.eval_expression(value, frame)?
                            else {
                                return Err(runtime_error(
                                    "checked object spread received a non-object",
                                    span,
                                ));
                            };
                            object.extend(fields);
                        }
                    }
                }
                Ok(ConfigValue::Section(object))
            }
            CompiledExpression::Operation {
                operation,
                operands,
                span,
            } => {
                if *operation == TypedOperation::Fallback {
                    return self
                        .eval_expression(&operands[0], frame)
                        .or_else(|_| self.eval_expression(&operands[1], frame));
                }
                let values = operands
                    .iter()
                    .map(|operand| self.eval_expression(operand, frame))
                    .collect::<Result<Vec<_>, _>>()?;
                eval_operation(*operation, &values, span)
            }
            CompiledExpression::Index {
                source,
                index,
                span,
            } => {
                let source = self.eval_expression(source, frame)?;
                let index = self.eval_expression(index, frame)?;
                match (source, index) {
                    (ConfigValue::List(items), ConfigValue::Int(index)) if index >= 0 => items
                        .get(index as usize)
                        .cloned()
                        .ok_or_else(|| runtime_error("list index is out of bounds", span)),
                    (source, index) => Err(runtime_error(
                        &format!(
                            "checked index received {} and {}",
                            source.type_name(),
                            index.type_name()
                        ),
                        span,
                    )),
                }
            }
            CompiledExpression::Field { base, field, span } => {
                let base = self.eval_expression(base, frame)?;
                let ConfigValue::Section(fields) = base else {
                    return Err(type_error("object", &base, span));
                };
                fields
                    .get(field)
                    .cloned()
                    .ok_or_else(|| runtime_error(&format!("object has no field '{field}'"), span))
            }
            CompiledExpression::Interpolation(parts, span) => {
                let mut output = String::new();
                for part in parts {
                    match part {
                        CompiledStringPart::Literal(value) => output.push_str(value),
                        CompiledStringPart::Expression(value) => {
                            let value = self.eval_expression(value, frame)?;
                            match value {
                                ConfigValue::Str(value) => output.push_str(&value),
                                ConfigValue::Int(value) => output.push_str(&value.to_string()),
                                ConfigValue::Float(value) => output.push_str(&value.to_string()),
                                ConfigValue::Bool(value) => output.push_str(&value.to_string()),
                                other => return Err(type_error("primitive", &other, span)),
                            }
                        }
                    }
                }
                Ok(ConfigValue::Str(output))
            }
            CompiledExpression::Comprehension {
                binding,
                source,
                body,
                span,
            } => {
                let ConfigValue::List(items) = self.eval_expression(source, frame)? else {
                    return Err(runtime_error(
                        "checked comprehension received a non-list",
                        span,
                    ));
                };
                let mut output = Vec::with_capacity(items.len());
                for item in items {
                    frame.write(*binding, item, span)?;
                    output.push(self.eval_expression(body, frame)?);
                }
                Ok(ConfigValue::List(output))
            }
            CompiledExpression::Shell(shell) => Ok(ConfigValue::Shell(
                crate::evaluator::lower_shell_expr(shell),
            )),
        }
    }
}

fn eval_operation(
    operation: TypedOperation,
    values: &[ConfigValue],
    span: &Span,
) -> Result<ConfigValue, SparError> {
    macro_rules! binary {
        ($left:pat, $right:pat => $value:expr) => {
            match values {
                [$left, $right] => Ok($value),
                _ => Err(operation_type_error(operation, values, span)),
            }
        };
    }
    match operation {
        TypedOperation::IntAdd => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Int(a + b))
        }
        TypedOperation::FloatAdd => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Float(a + b))
        }
        TypedOperation::StringConcat => {
            binary!(ConfigValue::Str(a), ConfigValue::Str(b) => ConfigValue::Str(format!("{a}{b}")))
        }
        TypedOperation::ShellConcat => {
            binary!(ConfigValue::Shell(a), ConfigValue::Shell(b) => ConfigValue::Shell(a.clone().then(b.clone())))
        }
        TypedOperation::IntSub => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Int(a - b))
        }
        TypedOperation::FloatSub => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Float(a - b))
        }
        TypedOperation::IntMul => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Int(a * b))
        }
        TypedOperation::FloatMul => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Float(a * b))
        }
        TypedOperation::IntDiv => match values {
            [ConfigValue::Int(_), ConfigValue::Int(0)] => Err(SparError::EvalError {
                message: "division by zero".into(),
                span: span.clone(),
            }),
            [ConfigValue::Int(a), ConfigValue::Int(b)] => Ok(ConfigValue::Int(a / b)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatDiv => match values {
            [ConfigValue::Float(_), ConfigValue::Float(b)] if *b == 0.0 => {
                Err(SparError::EvalError {
                    message: "division by zero".into(),
                    span: span.clone(),
                })
            }
            [ConfigValue::Float(a), ConfigValue::Float(b)] => Ok(ConfigValue::Float(a / b)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::IntEq => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Bool(a == b))
        }
        TypedOperation::FloatEq => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Bool(a == b))
        }
        TypedOperation::StringEq => {
            binary!(ConfigValue::Str(a), ConfigValue::Str(b) => ConfigValue::Bool(a == b))
        }
        TypedOperation::BoolEq => {
            binary!(ConfigValue::Bool(a), ConfigValue::Bool(b) => ConfigValue::Bool(a == b))
        }
        TypedOperation::IntNotEq => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Bool(a != b))
        }
        TypedOperation::FloatNotEq => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Bool(a != b))
        }
        TypedOperation::StringNotEq => {
            binary!(ConfigValue::Str(a), ConfigValue::Str(b) => ConfigValue::Bool(a != b))
        }
        TypedOperation::BoolNotEq => {
            binary!(ConfigValue::Bool(a), ConfigValue::Bool(b) => ConfigValue::Bool(a != b))
        }
        TypedOperation::IntLt => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Bool(a < b))
        }
        TypedOperation::FloatLt => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Bool(a < b))
        }
        TypedOperation::IntGt => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Bool(a > b))
        }
        TypedOperation::FloatGt => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Bool(a > b))
        }
        TypedOperation::IntLtEq => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Bool(a <= b))
        }
        TypedOperation::FloatLtEq => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Bool(a <= b))
        }
        TypedOperation::IntGtEq => {
            binary!(ConfigValue::Int(a), ConfigValue::Int(b) => ConfigValue::Bool(a >= b))
        }
        TypedOperation::FloatGtEq => {
            binary!(ConfigValue::Float(a), ConfigValue::Float(b) => ConfigValue::Bool(a >= b))
        }
        TypedOperation::BoolAnd => {
            binary!(ConfigValue::Bool(a), ConfigValue::Bool(b) => ConfigValue::Bool(*a && *b))
        }
        TypedOperation::BoolOr => {
            binary!(ConfigValue::Bool(a), ConfigValue::Bool(b) => ConfigValue::Bool(*a || *b))
        }
        TypedOperation::BoolNot => match values {
            [ConfigValue::Bool(value)] => Ok(ConfigValue::Bool(!value)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::IntNeg => match values {
            [ConfigValue::Int(value)] => Ok(ConfigValue::Int(-value)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatNeg => match values {
            [ConfigValue::Float(value)] => Ok(ConfigValue::Float(-value)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::Fallback => Err(runtime_error(
            "fallback reached eager operation dispatch",
            span,
        )),
    }
}

fn operation_type_error(
    operation: TypedOperation,
    values: &[ConfigValue],
    span: &Span,
) -> SparError {
    let types = values
        .iter()
        .map(ConfigValue::type_name)
        .collect::<Vec<_>>()
        .join(", ");
    runtime_error(
        &format!("checked operation {operation:?} received [{types}]"),
        span,
    )
}

fn type_error(expected: &str, value: &ConfigValue, span: &Span) -> SparError {
    runtime_error(
        &format!("expected {expected}, received {}", value.type_name()),
        span,
    )
}

fn module_state_error(span: &Span) -> SparError {
    runtime_error("module state is unavailable", span)
}

fn runtime_error(message: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("internal runtime error: {message}"),
        span: span.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_reads_and_writes_valid_slots() {
        let mut frame = Frame::new(1);
        frame
            .write(LocalSlot(0), ConfigValue::Int(7), &Span::dummy())
            .unwrap();
        assert_eq!(
            frame.read(LocalSlot(0), &Span::dummy()).unwrap(),
            &ConfigValue::Int(7)
        );
    }

    #[test]
    fn frame_rejects_invalid_and_uninitialized_slots() {
        let frame = Frame::new(1);
        let uninitialized = frame.read(LocalSlot(0), &Span::dummy()).unwrap_err();
        let invalid = frame.read(LocalSlot(1), &Span::dummy()).unwrap_err();
        assert!(uninitialized
            .to_string()
            .contains("internal runtime error:"));
        assert!(uninitialized.to_string().contains("uninitialized"));
        assert!(invalid.to_string().contains("internal runtime error:"));
        assert!(invalid.to_string().contains("invalid"));
    }
}
