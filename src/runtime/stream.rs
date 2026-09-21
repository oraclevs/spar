use std::collections::VecDeque;
use std::fmt;

use crate::ast::SparType;
use crate::error::{Span, SparError};

use super::Value;

pub type StreamPull = Box<dyn FnMut() -> Result<Option<Value>, SparError> + Send>;
pub type StreamCancel = Box<dyn FnMut() + Send>;
pub type StreamInvoke<'a> = dyn FnMut(&Value, Vec<Value>) -> Result<Value, SparError> + 'a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    Open,
    Complete,
    Cancelled,
    Failed,
}

enum StreamSource {
    Producer {
        pull: StreamPull,
        cancel: Option<StreamCancel>,
    },
    Map {
        upstream: Box<StreamResource>,
        mapper: Value,
    },
    Filter {
        upstream: Box<StreamResource>,
        predicate: Value,
    },
    Take {
        upstream: Box<StreamResource>,
        remaining: usize,
    },
    Skip {
        upstream: Box<StreamResource>,
        remaining: usize,
    },
    Unique {
        upstream: Box<StreamResource>,
        seen: Vec<Value>,
    },
    UniqueBy {
        upstream: Box<StreamResource>,
        key: Value,
        seen: Vec<Value>,
    },
    Flatten {
        upstream: Box<StreamResource>,
        buffered: VecDeque<Value>,
    },
    Select {
        upstream: Box<StreamResource>,
        fields: Vec<String>,
    },
}

/// Runtime-owned lazy structured sequence.
///
/// The stream itself lives in `RuntimeContext::resources()` and Spar values carry
/// only the corresponding `ResourceId`. That keeps iterators/producers out of
/// the cloneable `Value` graph while still allowing a static `Stream<T>` type.
/// Structured transforms wrap the upstream resource rather than materializing it.
pub struct StreamResource {
    element_type: SparType,
    source: StreamSource,
    state: StreamState,
}

impl fmt::Debug for StreamResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamResource")
            .field("element_type", &self.element_type)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl StreamResource {
    pub fn new(
        element_type: SparType,
        pull: impl FnMut() -> Result<Option<Value>, SparError> + Send + 'static,
    ) -> Self {
        Self {
            element_type,
            source: StreamSource::Producer {
                pull: Box::new(pull),
                cancel: None,
            },
            state: StreamState::Open,
        }
    }

    pub fn with_cancel(
        element_type: SparType,
        pull: impl FnMut() -> Result<Option<Value>, SparError> + Send + 'static,
        cancel: impl FnMut() + Send + 'static,
    ) -> Self {
        Self {
            element_type,
            source: StreamSource::Producer {
                pull: Box::new(pull),
                cancel: Some(Box::new(cancel)),
            },
            state: StreamState::Open,
        }
    }

    pub fn from_values(element_type: SparType, values: Vec<Value>) -> Self {
        let mut values = values.into_iter();
        Self::new(element_type, move || Ok(values.next()))
    }

    pub fn element_type(&self) -> &SparType {
        &self.element_type
    }

    pub fn state(&self) -> StreamState {
        self.state
    }

    pub fn map_lazy(self, element_type: SparType, mapper: Value) -> Self {
        Self {
            element_type,
            source: StreamSource::Map {
                upstream: Box::new(self),
                mapper,
            },
            state: StreamState::Open,
        }
    }

    pub fn filter_lazy(self, predicate: Value) -> Self {
        let element_type = self.element_type.clone();
        Self {
            element_type,
            source: StreamSource::Filter {
                upstream: Box::new(self),
                predicate,
            },
            state: StreamState::Open,
        }
    }

    pub fn take_lazy(self, count: usize) -> Self {
        let element_type = self.element_type.clone();
        Self {
            element_type,
            source: StreamSource::Take {
                upstream: Box::new(self),
                remaining: count,
            },
            state: StreamState::Open,
        }
    }

    pub fn skip_lazy(self, count: usize) -> Self {
        let element_type = self.element_type.clone();
        Self {
            element_type,
            source: StreamSource::Skip {
                upstream: Box::new(self),
                remaining: count,
            },
            state: StreamState::Open,
        }
    }

    pub fn unique_lazy(self) -> Self {
        let element_type = self.element_type.clone();
        Self {
            element_type,
            source: StreamSource::Unique {
                upstream: Box::new(self),
                seen: Vec::new(),
            },
            state: StreamState::Open,
        }
    }

    pub fn unique_by_lazy(self, key: Value) -> Self {
        let element_type = self.element_type.clone();
        Self {
            element_type,
            source: StreamSource::UniqueBy {
                upstream: Box::new(self),
                key,
                seen: Vec::new(),
            },
            state: StreamState::Open,
        }
    }

    pub fn flatten_lazy(self, element_type: SparType) -> Self {
        Self {
            element_type,
            source: StreamSource::Flatten {
                upstream: Box::new(self),
                buffered: VecDeque::new(),
            },
            state: StreamState::Open,
        }
    }

    pub fn select_lazy(self, fields: Vec<String>) -> Self {
        Self {
            element_type: SparType::Named("Record".into()),
            source: StreamSource::Select {
                upstream: Box::new(self),
                fields,
            },
            state: StreamState::Open,
        }
    }

    /// Pull one value from a primitive producer. Structured adapters that need
    /// to execute Spar callables must use `next_with` so the runtime can supply
    /// the callable executor.
    #[allow(clippy::should_implement_trait)] // fallible pull, not `Iterator::next`
    pub fn next(&mut self) -> Result<Option<Value>, SparError> {
        let mut reject = |_callable: &Value, _args: Vec<Value>| {
            Err(SparError::EvalError {
                message: "structured stream transform requires runtime callable execution".into(),
                span: Span::dummy(),
            })
        };
        self.next_with(&mut reject)
    }

    pub fn next_with(&mut self, invoke: &mut StreamInvoke<'_>) -> Result<Option<Value>, SparError> {
        if self.state != StreamState::Open {
            return Ok(None);
        }

        let result = self.pull_open(invoke);
        match result {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => {
                if self.state == StreamState::Open {
                    self.state = StreamState::Complete;
                }
                Ok(None)
            }
            Err(error) => {
                self.state = StreamState::Failed;
                Err(error)
            }
        }
    }

    fn pull_open(&mut self, invoke: &mut StreamInvoke<'_>) -> Result<Option<Value>, SparError> {
        match &mut self.source {
            StreamSource::Producer { pull, .. } => pull(),
            StreamSource::Map { upstream, mapper } => match upstream.next_with(invoke)? {
                Some(value) => invoke(mapper, vec![value]).map(Some),
                None => Ok(None),
            },
            StreamSource::Filter {
                upstream,
                predicate,
            } => loop {
                let Some(value) = upstream.next_with(invoke)? else {
                    return Ok(None);
                };
                match invoke(predicate, vec![value.clone()])? {
                    Value::Bool(true) => return Ok(Some(value)),
                    Value::Bool(false) => continue,
                    other => {
                        return Err(SparError::EvalError {
                            message: format!(
                                "stream predicate must return bool, found {}",
                                other.type_name()
                            ),
                            span: Span::dummy(),
                        })
                    }
                }
            },
            StreamSource::Take {
                upstream,
                remaining,
            } => {
                if *remaining == 0 {
                    upstream.cancel();
                    self.state = StreamState::Complete;
                    return Ok(None);
                }
                let next = upstream.next_with(invoke)?;
                match next {
                    Some(value) => {
                        *remaining -= 1;
                        if *remaining == 0 {
                            upstream.cancel();
                            self.state = StreamState::Complete;
                        }
                        Ok(Some(value))
                    }
                    None => Ok(None),
                }
            }
            StreamSource::Skip {
                upstream,
                remaining,
            } => {
                while *remaining > 0 {
                    if upstream.next_with(invoke)?.is_none() {
                        return Ok(None);
                    }
                    *remaining -= 1;
                }
                upstream.next_with(invoke)
            }
            StreamSource::Unique { upstream, seen } => loop {
                let Some(value) = upstream.next_with(invoke)? else {
                    return Ok(None);
                };
                ensure_comparable(&value)?;
                if seen.iter().any(|existing| existing == &value) {
                    continue;
                }
                seen.push(value.clone());
                return Ok(Some(value));
            },
            StreamSource::UniqueBy {
                upstream,
                key,
                seen,
            } => loop {
                let Some(value) = upstream.next_with(invoke)? else {
                    return Ok(None);
                };
                let computed = invoke(key, vec![value.clone()])?;
                ensure_comparable(&computed)?;
                if seen.iter().any(|existing| existing == &computed) {
                    continue;
                }
                seen.push(computed);
                return Ok(Some(value));
            },
            StreamSource::Flatten { upstream, buffered } => loop {
                if let Some(value) = buffered.pop_front() {
                    return Ok(Some(value));
                }
                let Some(value) = upstream.next_with(invoke)? else {
                    return Ok(None);
                };
                let Value::List(values) = value else {
                    return Err(SparError::EvalError {
                        message: "flatten expects sequence elements to be lists".into(),
                        span: Span::dummy(),
                    });
                };
                buffered.extend(values);
            },
            StreamSource::Select { upstream, fields } => {
                let Some(value) = upstream.next_with(invoke)? else {
                    return Ok(None);
                };
                project_record(value, fields).map(Some)
            }
        }
    }

    pub fn cancel(&mut self) {
        if self.state != StreamState::Open && self.state != StreamState::Failed {
            return;
        }
        self.state = StreamState::Cancelled;
        self.cancel_source();
    }

    fn cancel_source(&mut self) {
        match &mut self.source {
            StreamSource::Producer { cancel, .. } => {
                if let Some(mut cancel) = cancel.take() {
                    cancel();
                }
            }
            StreamSource::Map { upstream, .. }
            | StreamSource::Filter { upstream, .. }
            | StreamSource::Take { upstream, .. }
            | StreamSource::Skip { upstream, .. }
            | StreamSource::Unique { upstream, .. }
            | StreamSource::UniqueBy { upstream, .. }
            | StreamSource::Flatten { upstream, .. }
            | StreamSource::Select { upstream, .. } => upstream.cancel(),
        }
    }

    /// Pulls at most `count` values. This is intentionally a runtime helper;
    /// user-facing `take` is implemented by the structured-data transforms.
    pub fn take_values(&mut self, count: usize) -> Result<Vec<Value>, SparError> {
        let mut values = Vec::with_capacity(count);
        while values.len() < count {
            let Some(value) = self.next()? else {
                break;
            };
            values.push(value);
        }
        Ok(values)
    }
}

impl Drop for StreamResource {
    fn drop(&mut self) {
        match self.state {
            StreamState::Open | StreamState::Failed => self.cancel(),
            StreamState::Complete | StreamState::Cancelled => {}
        }
    }
}

fn ensure_comparable(value: &Value) -> Result<(), SparError> {
    if value.is_data_comparable() {
        Ok(())
    } else {
        Err(SparError::EvalError {
            message: format!(
                "{} values cannot be compared for structured uniqueness",
                value.type_name()
            ),
            span: Span::dummy(),
        })
    }
}

fn project_record(value: Value, fields: &[String]) -> Result<Value, SparError> {
    let Value::Object(values) = value else {
        return Err(SparError::EvalError {
            message: "select expects Record rows".into(),
            span: Span::dummy(),
        });
    };
    let mut selected = indexmap::IndexMap::new();
    for field in fields {
        let value = values.get(field).ok_or_else(|| SparError::EvalError {
            message: format!("record has no field '{field}'"),
            span: Span::dummy(),
        })?;
        selected.insert(field.clone(), value.clone());
    }
    Ok(Value::Object(selected))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    use crate::error::Span;

    use super::*;

    fn counting_stream(pulls: Arc<AtomicUsize>) -> StreamResource {
        let mut value = 0_i64;
        StreamResource::new(SparType::Int, move || {
            pulls.fetch_add(1, Ordering::SeqCst);
            value += 1;
            Ok(Some(Value::Int(value)))
        })
    }

    #[test]
    fn take_zero_does_not_pull_upstream() {
        let pulls = Arc::new(AtomicUsize::new(0));
        let mut stream = counting_stream(pulls.clone());

        assert!(stream.take_values(0).unwrap().is_empty());
        assert_eq!(pulls.load(Ordering::SeqCst), 0);
        assert_eq!(stream.state(), StreamState::Open);
    }

    #[test]
    fn take_three_pulls_only_three_values() {
        let pulls = Arc::new(AtomicUsize::new(0));
        let mut stream = counting_stream(pulls.clone());

        assert_eq!(
            stream.take_values(3).unwrap(),
            vec![Value::Int(1), Value::Int(2), Value::Int(3)]
        );
        assert_eq!(pulls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn filtered_take_three_pulls_only_until_three_matches() {
        let pulls = Arc::new(AtomicUsize::new(0));
        let mut upstream = counting_stream(pulls.clone());
        let mut filtered = StreamResource::new(SparType::Int, move || loop {
            match upstream.next()? {
                Some(Value::Int(value)) if value % 2 == 0 => {
                    return Ok(Some(Value::Int(value)));
                }
                Some(_) => continue,
                None => return Ok(None),
            }
        });

        assert_eq!(
            filtered.take_values(3).unwrap(),
            vec![Value::Int(2), Value::Int(4), Value::Int(6)]
        );
        assert_eq!(pulls.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn completion_is_sticky_and_does_not_repull() {
        let pulls = Arc::new(AtomicUsize::new(0));
        let seen = pulls.clone();
        let mut emitted = false;
        let mut stream = StreamResource::new(SparType::Int, move || {
            seen.fetch_add(1, Ordering::SeqCst);
            if emitted {
                Ok(None)
            } else {
                emitted = true;
                Ok(Some(Value::Int(1)))
            }
        });

        assert_eq!(stream.next().unwrap(), Some(Value::Int(1)));
        assert_eq!(stream.next().unwrap(), None);
        assert_eq!(stream.next().unwrap(), None);
        assert_eq!(pulls.load(Ordering::SeqCst), 2);
        assert_eq!(stream.state(), StreamState::Complete);
    }

    #[test]
    fn resource_table_clear_drops_and_cancels_stream() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let marker = cancelled.clone();
        let mut resources = crate::runtime::resource::ResourceTable::new();
        resources.insert(StreamResource::with_cancel(
            SparType::Int,
            || Ok(Some(Value::Int(1))),
            move || marker.store(true, Ordering::SeqCst),
        ));

        assert!(!cancelled.load(Ordering::SeqCst));
        resources.clear();
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn drop_invokes_cancellation_hook_for_open_stream() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let marker = cancelled.clone();
        {
            let _stream = StreamResource::with_cancel(
                SparType::Int,
                || Ok(Some(Value::Int(1))),
                move || marker.store(true, Ordering::SeqCst),
            );
        }
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn failures_transition_stream_to_failed_state() {
        let mut stream = StreamResource::new(SparType::Int, || {
            Err(SparError::EvalError {
                message: "boom".into(),
                span: Span::dummy(),
            })
        });

        assert!(stream.next().is_err());
        assert_eq!(stream.state(), StreamState::Failed);
        assert_eq!(stream.next().unwrap(), None);
    }

    #[test]
    fn mapped_stream_is_lazy_and_invokes_mapper_only_when_pulled() {
        let pulls = Arc::new(AtomicUsize::new(0));
        let mapped = Arc::new(AtomicUsize::new(0));
        let upstream = counting_stream(pulls.clone());
        let mapped_count = mapped.clone();
        let mut stream = upstream.map_lazy(
            SparType::Int,
            Value::Function(crate::compiled::FunctionId(0)),
        );
        let mut invoke = move |_callable: &Value, args: Vec<Value>| {
            mapped_count.fetch_add(1, Ordering::SeqCst);
            let Value::Int(value) = &args[0] else {
                unreachable!()
            };
            Ok(Value::Int(*value * 10))
        };

        assert_eq!(pulls.load(Ordering::SeqCst), 0);
        assert_eq!(mapped.load(Ordering::SeqCst), 0);
        assert_eq!(stream.next_with(&mut invoke).unwrap(), Some(Value::Int(10)));
        assert_eq!(pulls.load(Ordering::SeqCst), 1);
        assert_eq!(mapped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn filtered_stream_take_three_stops_after_three_matches() {
        let pulls = Arc::new(AtomicUsize::new(0));
        let upstream = counting_stream(pulls.clone());
        let callable = Value::Function(crate::compiled::FunctionId(0));
        let mut stream = upstream.filter_lazy(callable).take_lazy(3);
        let mut invoke = |_callable: &Value, args: Vec<Value>| {
            let Value::Int(value) = &args[0] else {
                unreachable!()
            };
            Ok(Value::Bool(*value % 2 == 0))
        };

        let mut values = Vec::new();
        while let Some(value) = stream.next_with(&mut invoke).unwrap() {
            values.push(value);
        }

        assert_eq!(values, vec![Value::Int(2), Value::Int(4), Value::Int(6)]);
        assert_eq!(pulls.load(Ordering::SeqCst), 6);
        assert_eq!(stream.state(), StreamState::Complete);
    }

    #[test]
    fn stream_take_zero_never_pulls_wrapped_upstream() {
        let pulls = Arc::new(AtomicUsize::new(0));
        let upstream = counting_stream(pulls.clone());
        let mut stream = upstream.take_lazy(0);
        let mut invoke = |_callable: &Value, _args: Vec<Value>| unreachable!();

        assert_eq!(stream.next_with(&mut invoke).unwrap(), None);
        assert_eq!(pulls.load(Ordering::SeqCst), 0);
        assert_eq!(stream.state(), StreamState::Complete);
    }

    #[test]
    fn dropping_failed_stream_invokes_cancellation_hook_once() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        let marker = cancellations.clone();
        {
            let mut stream = StreamResource::with_cancel(
                SparType::Int,
                || {
                    Err(SparError::EvalError {
                        message: "boom".into(),
                        span: Span::dummy(),
                    })
                },
                move || {
                    marker.fetch_add(1, Ordering::SeqCst);
                },
            );

            assert!(stream.next().is_err());
            assert_eq!(stream.state(), StreamState::Failed);
        }

        assert_eq!(cancellations.load(Ordering::SeqCst), 1);
    }
}
