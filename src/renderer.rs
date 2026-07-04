use crate::error::SparError;

pub struct ErrorRenderer<'a> {
    pub source:   &'a str,
    pub filename: &'a str,
    pub colored:  bool,
}

impl<'a> ErrorRenderer<'a> {
    /// Plain-text renderer — all existing tests use this.
    pub fn new(source: &'a str, filename: &'a str) -> Self {
        Self { source, filename, colored: false }
    }

    /// Renderer with ANSI colour codes for terminal output.
    pub fn with_color(source: &'a str, filename: &'a str) -> Self {
        Self { source, filename, colored: true }
    }

    pub fn render(&self, error: &SparError) -> String {
        let (code, message, span, hint) = match error {
            SparError::LexError    { message, span }         => ("lex",     message, span, None),
            SparError::ParseError  { message, span }         => ("parse",   message, span, None),
            SparError::ResolveError{ message, span, hint }   => ("resolve", message, span, hint.as_ref()),
            SparError::TypeError   { message, span, hint }   => ("type",    message, span, hint.as_ref()),
            SparError::EvalError   { message, span }         => ("eval",    message, span, None),
            SparError::SchemaError { message, span }         => ("schema",  message, span, None),
        };

        let line_text = self.source
            .lines()
            .nth(span.line.saturating_sub(1) as usize)
            .unwrap_or("");

        let line_no_str = span.line.to_string();
        let w           = line_no_str.len();
        let pad         = " ".repeat(w);

        let col_offset  = span.col.saturating_sub(1) as usize;
        let caret_width = span.end.saturating_sub(span.start).max(1);
        let caret_body  = format!("{}{}", " ".repeat(col_offset), "^".repeat(caret_width));

        let mut out = if self.colored {
            format!(
                "\x1b[1;31merror[{code}]\x1b[0m: {message}\n\
                 \x1b[34m{pad} --> {filename}:{line}:{col}\x1b[0m\n\
                 {pad}  |\n\
                 {line_no:<w$}  |  {line_text}\n\
                 {pad}  |  \x1b[1;31m{caret_body}\x1b[0m",
                filename = self.filename,
                line     = span.line,
                col      = span.col,
                line_no  = line_no_str,
                w        = w,
            )
        } else {
            format!(
                "error[{code}]: {message}\n\
                 {pad} --> {filename}:{line}:{col}\n\
                 {pad}  |\n\
                 {line_no:<w$}  |  {line_text}\n\
                 {pad}  |  {caret_body}",
                filename = self.filename,
                line     = span.line,
                col      = span.col,
                line_no  = line_no_str,
                w        = w,
            )
        };

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
}
