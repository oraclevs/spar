#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
    /// `0` = the source being compiled; otherwise an id from `source_map`.
    pub file: u32,
}

impl Span {
    pub fn new(start: usize, end: usize, line: u32, col: u32) -> Self {
        Self {
            start,
            end,
            line,
            col,
            file: 0,
        }
    }

    pub fn dummy() -> Self {
        Self {
            start: 0,
            end: 0,
            line: 0,
            col: 0,
            file: 0,
        }
    }

    pub fn with_file(mut self, file: u32) -> Self {
        self.file = file;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StackFrame {
    pub function: String,
    pub location: Span,
}

/// Standard stack trace. Frames are innermost first; `pending_call` is the
/// call span the next frame will use as its location.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StackTrace {
    pub frames: Vec<StackFrame>,
    pub pending_call: Option<Span>,
    /// The next function the error leaves never started (its arguments
    /// failed), so `RuntimeFault::leave_function` skips it once.
    pub suppress_next_leave: bool,
    /// Set by the skipped leave above: the call-site note that follows is
    /// skipped too, since the call never happened.
    pub suppress_next_note: bool,
}

pub enum FrameLine<'a> {
    Frame(&'a StackFrame),
    Elided(usize),
    /// The call that entered the outermost frame from top-level code.
    TopLevel(&'a Span),
}

const TRACE_HEAD: usize = 5;
const TRACE_TAIL: usize = 4;
const TRACE_MAX: usize = TRACE_HEAD + 1 + TRACE_TAIL;

impl StackTrace {
    pub fn note_call(&mut self, call_span: Span) {
        self.pending_call = Some(call_span);
    }

    pub fn push_frame(&mut self, function: &str, error_span: &Span) {
        let location = self
            .pending_call
            .take()
            .unwrap_or_else(|| error_span.clone());
        self.frames.push(StackFrame {
            function: function.to_string(),
            location,
        });
    }

    pub fn display_lines(&self) -> Vec<FrameLine<'_>> {
        let mut lines: Vec<FrameLine<'_>> = if self.frames.len() <= TRACE_MAX {
            self.frames.iter().map(FrameLine::Frame).collect()
        } else {
            let hidden = self.frames.len() - TRACE_HEAD - TRACE_TAIL;
            let mut lines: Vec<FrameLine<'_>> = self.frames[..TRACE_HEAD]
                .iter()
                .map(FrameLine::Frame)
                .collect();
            lines.push(FrameLine::Elided(hidden));
            lines.extend(
                self.frames[self.frames.len() - TRACE_TAIL..]
                    .iter()
                    .map(FrameLine::Frame),
            );
            lines
        };
        if let Some(span) = &self.pending_call {
            if span.line != 0 {
                lines.push(FrameLine::TopLevel(span));
            }
        }
        lines
    }
}

#[derive(Debug, Clone)]
pub struct TracedError {
    pub error: SparError,
    pub trace: StackTrace,
}

/// Message for a command that could not be started. `NotFound` reads as
/// "not found" instead of the raw OS error.
pub(crate) fn shell_spawn_message(program: &str, error: &std::io::Error) -> String {
    // A custom NotFound (e.g. "`jobs` is a Spar shell builtin...") keeps its
    // own text; only a real OS "no such file" reads as plain "not found".
    if error.kind() == std::io::ErrorKind::NotFound && error.raw_os_error().is_some() {
        format!("could not run '{program}': not found")
    } else {
        format!("could not run '{program}': {error}")
    }
}

#[derive(Debug, Clone)]
pub enum SparError {
    LexError {
        message: String,
        span: Span,
    },
    ParseError {
        message: String,
        span: Span,
    },
    ResolveError {
        message: String,
        hint: Option<String>,
        span: Span,
    },
    TypeError {
        message: String,
        hint: Option<String>,
        span: Span,
    },
    EvalError {
        message: String,
        span: Span,
    },
    SchemaError {
        message: String,
        span: Span,
    },
    /// An error carrying the call stack it unwound through. Transparent to
    /// `span()`, `span_mut()` and `Display`; use `base()` to match on the kind.
    Traced(Box<TracedError>),
}

impl SparError {
    pub fn base(&self) -> &SparError {
        match self {
            SparError::Traced(inner) => inner.error.base(),
            other => other,
        }
    }

    pub fn trace(&self) -> Option<&StackTrace> {
        match self {
            SparError::Traced(inner) => Some(&inner.trace),
            _ => None,
        }
    }

    pub fn traced_mut(&mut self) -> &mut StackTrace {
        if !matches!(self, SparError::Traced(_)) {
            let error = std::mem::replace(
                self,
                SparError::EvalError {
                    message: String::new(),
                    span: Span::dummy(),
                },
            );
            *self = SparError::Traced(Box::new(TracedError {
                error,
                trace: StackTrace::default(),
            }));
        }
        match self {
            SparError::Traced(inner) => &mut inner.trace,
            _ => unreachable!("wrapped above"),
        }
    }

    pub fn span(&self) -> &Span {
        match self {
            SparError::LexError { span, .. }
            | SparError::ParseError { span, .. }
            | SparError::ResolveError { span, .. }
            | SparError::TypeError { span, .. }
            | SparError::EvalError { span, .. }
            | SparError::SchemaError { span, .. } => span,
            SparError::Traced(inner) => inner.error.span(),
        }
    }

    pub fn span_mut(&mut self) -> &mut Span {
        match self {
            SparError::LexError { span, .. }
            | SparError::ParseError { span, .. }
            | SparError::ResolveError { span, .. }
            | SparError::TypeError { span, .. }
            | SparError::EvalError { span, .. }
            | SparError::SchemaError { span, .. } => span,
            SparError::Traced(inner) => inner.error.span_mut(),
        }
    }
}

impl std::fmt::Display for SparError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SparError::LexError { message, span } => {
                write!(f, "error[lex] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::ParseError { message, span } => {
                write!(
                    f,
                    "error[parse] at {}:{} — {}",
                    span.line, span.col, message
                )
            }
            SparError::ResolveError { message, span, .. } => {
                write!(
                    f,
                    "error[resolve] at {}:{} — {}",
                    span.line, span.col, message
                )
            }
            SparError::TypeError { message, span, .. } => {
                write!(f, "error[type] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::EvalError { message, span } => {
                write!(f, "error[eval] at {}:{} — {}", span.line, span.col, message)
            }
            SparError::SchemaError { message, span } => {
                write!(
                    f,
                    "error[schema] at {}:{} — {}",
                    span.line, span.col, message
                )
            }
            SparError::Traced(inner) => inner.error.fmt(f),
        }
    }
}

impl std::error::Error for SparError {}

#[cfg(test)]
mod trace_tests {
    use super::*;

    fn eval(span: Span) -> SparError {
        SparError::EvalError { message: "boom".into(), span }
    }

    fn sp(line: u32) -> Span {
        Span::new(0, 1, line, 1)
    }

    #[test]
    fn first_frame_uses_the_error_span_then_call_spans() {
        let mut error = eval(sp(2));
        let error_span = error.span().clone();
        let trace = error.traced_mut();
        trace.push_frame("boom", &error_span);
        trace.note_call(sp(5));
        trace.push_frame("main", &error_span);
        let frames = &error.trace().unwrap().frames;
        assert_eq!(frames[0], StackFrame { function: "boom".into(), location: sp(2) });
        assert_eq!(frames[1], StackFrame { function: "main".into(), location: sp(5) });
    }

    #[test]
    fn traced_errors_are_transparent_to_span_and_base() {
        let mut error = eval(sp(7));
        error.traced_mut().note_call(sp(1));
        assert_eq!(error.span(), &sp(7));
        assert!(matches!(error.base(), SparError::EvalError { .. }));
        assert!(error.to_string().contains("boom"));
    }

    #[test]
    fn short_traces_show_every_frame() {
        let mut trace = StackTrace::default();
        for n in 0..10 {
            trace.push_frame(&format!("f{n}"), &sp(n as u32 + 1));
        }
        assert_eq!(trace.display_lines().len(), 10);
        assert!(!trace.display_lines().iter().any(|l| matches!(l, FrameLine::Elided(_))));
    }

    #[test]
    fn long_traces_keep_five_marker_four() {
        let mut trace = StackTrace::default();
        for n in 0..30 {
            trace.push_frame(&format!("f{n}"), &sp(n as u32 + 1));
        }
        let lines = trace.display_lines();
        assert_eq!(lines.len(), 10);
        assert!(matches!(lines[0], FrameLine::Frame(f) if f.function == "f0"));
        assert!(matches!(lines[4], FrameLine::Frame(f) if f.function == "f4"));
        assert!(matches!(lines[5], FrameLine::Elided(21)));
        assert!(matches!(lines[6], FrameLine::Frame(f) if f.function == "f26"));
        assert!(matches!(lines[9], FrameLine::Frame(f) if f.function == "f29"));
    }
}
