use crate::error::{FrameLine, Span, SparError};

pub struct ErrorRenderer<'a> {
    pub source: &'a str,
    pub filename: &'a str,
    pub colored: bool,
}

impl<'a> ErrorRenderer<'a> {
    /// Plain-text renderer — all existing tests use this.
    pub fn new(source: &'a str, filename: &'a str) -> Self {
        Self {
            source,
            filename,
            colored: false,
        }
    }

    /// Renderer with ANSI colour codes for terminal output.
    pub fn with_color(source: &'a str, filename: &'a str) -> Self {
        Self {
            source,
            filename,
            colored: true,
        }
    }

    pub fn render(&self, error: &SparError) -> String {
        let (error, trace) = match error {
            SparError::Traced(inner) => (inner.error.base(), Some(&inner.trace)),
            other => (other, None),
        };
        let (code, message, span, hint) = match error {
            SparError::LexError { message, span } => ("lex", message, span, None),
            SparError::ParseError { message, span } => ("parse", message, span, None),
            SparError::ResolveError {
                message,
                span,
                hint,
            } => ("resolve", message, span, hint.as_ref()),
            SparError::TypeError {
                message,
                span,
                hint,
            } => ("type", message, span, hint.as_ref()),
            SparError::EvalError { message, span } => ("eval", message, span, None),
            SparError::SchemaError { message, span } => ("schema", message, span, None),
            SparError::Traced(_) => unreachable!("unwrapped above"),
        };

        let imported = if span.file != 0 {
            crate::source_map::lookup(span.file)
        } else {
            None
        };
        let (source_text, filename): (&str, &str) = match &imported {
            Some(file) => (&file.text, &file.path),
            None => (self.source, self.filename),
        };

        let line_text = source_text
            .lines()
            .nth(span.line.saturating_sub(1) as usize)
            .unwrap_or("");

        let line_no_str = span.line.to_string();
        let w = line_no_str.len();
        let pad = " ".repeat(w);

        let col_offset = span.col.saturating_sub(1) as usize;
        let caret_width = span.end.saturating_sub(span.start).max(1);
        let caret_body = format!("{}{}", " ".repeat(col_offset), "^".repeat(caret_width));

        let mut out = if self.colored {
            format!(
                "\x1b[1;31merror[{code}]\x1b[0m: {message}\n\
                 \x1b[34m{pad} --> {filename}:{line}:{col}\x1b[0m\n\
                 {pad}  |\n\
                 {line_no:<w$}  |  {line_text}\n\
                 {pad}  |  \x1b[1;31m{caret_body}\x1b[0m",
                filename = filename,
                line = span.line,
                col = span.col,
                line_no = line_no_str,
                w = w,
            )
        } else {
            format!(
                "error[{code}]: {message}\n\
                 {pad} --> {filename}:{line}:{col}\n\
                 {pad}  |\n\
                 {line_no:<w$}  |  {line_text}\n\
                 {pad}  |  {caret_body}",
                filename = filename,
                line = span.line,
                col = span.col,
                line_no = line_no_str,
                w = w,
            )
        };

        if let Some(trace) = trace {
            let lines = trace.display_lines();
            if !lines.is_empty() {
                out.push_str("\n\ntrace (most recent call first):");
                let mut first = true;
                for line in lines {
                    match line {
                        FrameLine::Frame(frame) => {
                            let verb = if first { "in" } else { "called from" };
                            first = false;
                            out.push_str(&format!(
                                "\n  {verb} {:<16} {}",
                                crate::naming::demangle(&frame.function),
                                location_text(&frame.location, self.filename)
                            ));
                        }
                        FrameLine::Elided(count) => {
                            out.push_str(&format!("\n  ... {count} more frames ..."));
                        }
                        FrameLine::TopLevel(span) => {
                            out.push_str(&format!(
                                "\n  called from top level    {}",
                                location_text(span, self.filename)
                            ));
                        }
                    }
                }
            }
        }

        if let Some(h) = hint {
            if self.colored {
                out.push_str(&format!("\n{pad}  |\n\x1b[33m{pad}  = help: {h}\x1b[0m"));
            } else {
                out.push_str(&format!("\n{pad}  |\n{pad}  = help: {h}"));
            }
        }

        out
    }

    pub fn render_all(&self, errors: &[SparError]) -> String {
        errors
            .iter()
            .map(|e| self.render(e))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

fn location_text(span: &Span, entry_filename: &str) -> String {
    if span.line == 0 {
        return String::new();
    }
    let name = match crate::source_map::lookup(span.file) {
        Some(file) => file.path,
        None => entry_filename.to_string(),
    };
    format!("{name}:{}:{}", span.line, span.col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Span;

    fn make_error(message: &str, line: u32, col: u32, start: usize, end: usize) -> SparError {
        SparError::ResolveError {
            message: message.into(),
            hint: None,
            span: Span::new(start, end, line, col),
        }
    }

    fn make_error_with_hint(
        message: &str,
        hint: &str,
        line: u32,
        col: u32,
        start: usize,
        end: usize,
    ) -> SparError {
        SparError::ResolveError {
            message: message.into(),
            hint: Some(hint.into()),
            span: Span::new(start, end, line, col),
        }
    }

    #[test]
    fn test_render_contains_source_line() {
        let src = "var port: int = 3000;";
        let r = ErrorRenderer::new(src, "test.spar");
        let e = make_error("test error", 1, 5, 4, 8);
        let out = r.render(&e);
        assert!(out.contains("var port: int = 3000;"), "got:\n{out}");
    }

    #[test]
    fn test_render_contains_error_code_and_message() {
        let src = "var port: int = 3000;";
        let r = ErrorRenderer::new(src, "test.spar");
        let e = make_error("something went wrong", 1, 5, 4, 8);
        let out = r.render(&e);
        assert!(out.contains("error[resolve]"), "got:\n{out}");
        assert!(out.contains("something went wrong"), "got:\n{out}");
    }

    #[test]
    fn test_render_contains_location() {
        let src = "hello\nworld\nthird line";
        let r = ErrorRenderer::new(src, "test.spar");
        let e = make_error("err", 3, 10, 17, 22);
        let out = r.render(&e);
        assert!(out.contains("test.spar:3:10"), "got:\n{out}");
    }

    #[test]
    fn test_render_caret() {
        let src = "var port: int = 3000;";
        let r = ErrorRenderer::new(src, "test.spar");
        // start=4, end=8 → caret_width=4
        let e = make_error("err", 1, 5, 4, 8);
        let out = r.render(&e);
        assert!(out.contains("^^^^"), "got:\n{out}");
    }

    #[test]
    fn test_render_hint_present() {
        let src = "var port: int = 3000;";
        let r = ErrorRenderer::new(src, "test.spar");
        let e = make_error_with_hint("err", "did you mean `port`?", 1, 5, 4, 8);
        let out = r.render(&e);
        assert!(out.contains("= help: did you mean"), "got:\n{out}");
    }

    #[test]
    fn test_render_no_hint() {
        let src = "var port: int = 3000;";
        let r = ErrorRenderer::new(src, "test.spar");
        let e = make_error("err", 1, 5, 4, 8);
        let out = r.render(&e);
        assert!(!out.contains("= help:"), "got:\n{out}");
    }

    #[test]
    fn span_from_an_imported_file_renders_against_that_file() {
        let id = crate::source_map::register(
            "/tmp/render-lib.spar",
            "fn boom(x: int) -> int {\n    return 10 / x;\n};\n",
        );
        let error = SparError::EvalError {
            message: "division by zero".into(),
            span: Span::new(36, 42, 2, 12).with_file(id),
        };
        let out = ErrorRenderer::new("entry source line one\nentry two\n", "script.spar")
            .render(&error);
        assert!(out.contains("/tmp/render-lib.spar:2:12"), "{out}");
        assert!(out.contains("return 10 / x;"), "{out}");
        assert!(!out.contains("entry two"), "{out}");
        assert!(!out.contains("script.spar"), "{out}");
    }

    #[test]
    fn file_zero_still_uses_the_renderer_source() {
        let error = SparError::EvalError {
            message: "m".into(),
            span: Span::new(0, 1, 1, 1),
        };
        let out = ErrorRenderer::new("abc\n", "entry.spar").render(&error);
        assert!(out.contains("entry.spar:1:1"), "{out}");
        assert!(out.contains("abc"), "{out}");
    }

    #[test]
    fn trace_prints_below_the_error_and_skips_dummy_locations() {
        let mut error = SparError::EvalError {
            message: "division by zero".into(),
            span: Span::new(0, 1, 1, 1),
        };
        let at = error.span().clone();
        let trace = error.traced_mut();
        trace.push_frame("boom", &at);
        trace.note_call(Span::new(0, 1, 4, 5));
        trace.push_frame("main", &at);
        trace.note_call(Span::dummy());
        trace.push_frame("<anonymous>", &at);
        let out = ErrorRenderer::new("abc\n", "entry.spar").render(&error);
        assert!(out.contains("trace (most recent call first):"), "{out}");
        assert!(out.contains("in boom"), "{out}");
        assert!(out.contains("called from main"), "{out}");
        assert!(!out.contains(":0:0"), "{out}");
    }

    #[test]
    fn errors_without_frames_print_no_trace_section() {
        let error = SparError::EvalError {
            message: "m".into(),
            span: Span::new(0, 1, 1, 1),
        };
        let out = ErrorRenderer::new("abc\n", "entry.spar").render(&error);
        assert!(!out.contains("trace"), "{out}");
    }
}
