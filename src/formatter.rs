use crate::ast::*;
use crate::error::SparError;
use crate::lexer::{CommentTrivia, Lexer};
use crate::parser::Parser;

pub struct FormatConfig {
    pub indent_width: usize,
    /// Comment trivia being re-attached while formatting a whole source
    /// file. Shared (not copied) so expression-level formatting, which only
    /// sees the config, consumes comments from the same position as the
    /// statement-level walk.
    comments: CommentCursor,
}

impl Default for FormatConfig {
    fn default() -> Self {
        FormatConfig {
            indent_width: 4,
            comments: CommentCursor::default(),
        }
    }
}

/// Canonical single-expression rendering, for tooling that shows a value
/// (e.g. a parameter default in signature help) without slicing source text.
pub fn format_expression(expr: &crate::ast::Expr) -> String {
    let mut out = String::new();
    format_expr(expr, 0, 0, &FormatConfig::default(), &mut out);
    out
}

pub fn format_source(src: &str) -> Result<String, SparError> {
    let lexer = Lexer::new(src);
    let shebang = lexer.shebang().map(str::to_owned);
    let (tokens, comments) = lexer.tokenize_with_comments()?;
    let mut program = Parser::new(tokens).parse()?;
    program.shebang = shebang;
    let formatted = format_program_with_comments(&program, &FormatConfig::default(), &comments);

    // Formatting must never hand the caller source that Spar itself cannot
    // parse. This is a final safety barrier around the pretty-printer: even
    // if a future AST branch is formatted incorrectly, `spar format` fails
    // instead of overwriting a valid file with invalid output.
    let formatted_tokens = Lexer::new(&formatted).tokenize()?;
    Parser::new(formatted_tokens).parse()?;

    Ok(formatted)
}

pub fn format_program(program: &Program, config: &FormatConfig) -> String {
    format_program_with_comments(program, config, &[])
}

pub fn format_program_with_comments(
    program: &Program,
    config: &FormatConfig,
    comments: &[CommentTrivia],
) -> String {
    let mut out = String::new();
    let mut cx = CommentCursor::new(comments);
    let config = &FormatConfig {
        indent_width: config.indent_width,
        comments: cx.clone(),
    };

    if let Some(shebang) = &program.shebang {
        out.push_str(shebang);
        out.push('\n');
    }
    if let Some(path) = &program.load_env {
        if path == ".env" {
            out.push_str("@LoadEnv\n");
        } else {
            out.push_str("@LoadEnv(\"");
            out.push_str(&escape_string_content(path));
            out.push_str("\")\n");
        }
    }
    if program.is_schema_file {
        out.push_str("@SchemaFile\n");
    }

    for (i, item) in program.items.iter().enumerate() {
        let item_line = item_span_line(item);
        let needs_separator = if i == 0 {
            program.shebang.is_some() || program.load_env.is_some() || program.is_schema_file
        } else {
            top_level_items_need_blank_line(&program.items[i - 1], item)
        };
        if needs_separator {
            out.push('\n');
        }
        // Emit any standalone comments preceding this item (after the blank-line separator)
        cx.emit_before_line(item_line, 0, config, &mut out);
        format_top_level_item(item, config, &mut cx, &mut out);
        cx.append_trailing(item_end_line(item), &mut out);
    }
    // Emit any trailing comments at end of file
    cx.emit_before_line(u32::MAX, 0, config, &mut out);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn top_level_items_need_blank_line(previous: &TopLevelItem, current: &TopLevelItem) -> bool {
    !matches!(
        (previous, current),
        (TopLevelItem::Import(_), TopLevelItem::Import(_))
    )
}

/// A position in the file's comment trivia. Cloning shares the position, so
/// every clone sees comments the others already placed.
#[derive(Clone, Default)]
struct CommentCursor {
    comments: std::rc::Rc<Vec<CommentTrivia>>,
    next: std::rc::Rc<std::cell::Cell<usize>>,
    /// Non-zero while formatting speculatively (e.g. trying an expression
    /// on one line); comments must not be consumed by a throwaway attempt.
    suspended: std::rc::Rc<std::cell::Cell<u32>>,
}

struct SuspendComments(CommentCursor);

impl Drop for SuspendComments {
    fn drop(&mut self) {
        self.0.suspended.set(self.0.suspended.get() - 1);
    }
}

impl CommentCursor {
    fn new(comments: &[CommentTrivia]) -> Self {
        Self {
            comments: std::rc::Rc::new(comments.to_vec()),
            next: std::rc::Rc::default(),
            suspended: std::rc::Rc::default(),
        }
    }

    fn suspend(&self) -> SuspendComments {
        self.suspended.set(self.suspended.get() + 1);
        SuspendComments(self.clone())
    }

    /// Emit all pending comments whose source line < `before_line`, as
    /// standalone lines. A comment lexed as trailing (on the same line as a
    /// prior token) is meant to be claimed by a `take_trailing` call closer
    /// to the field/item it followed — but if nothing claimed it before the
    /// cursor sweeps past here, it must still be emitted rather than
    /// silently dropped. A formatter must never delete a comment.
    fn emit_before_line(
        &self,
        before_line: u32,
        depth: usize,
        config: &FormatConfig,
        out: &mut String,
    ) {
        if self.suspended.get() > 0 {
            return;
        }
        while let Some(c) = self.comments.get(self.next.get()) {
            if c.line >= before_line {
                break;
            }
            let ind = indent(depth, config);
            out.push_str(&ind);
            out.push_str(&c.text);
            out.push('\n');
            self.next.set(self.next.get() + 1);
        }
    }

    /// Whether a pending comment starts inside byte range `[start, end)`.
    fn has_comment_in_offsets(&self, start: usize, end: usize) -> bool {
        self.comments
            .iter()
            .skip(self.next.get())
            .take_while(|comment| comment.start < end)
            .any(|comment| comment.start >= start)
    }

    /// Whether a pending comment sits on a line in `[from_line, before_line)`.
    fn has_comment_in_lines(&self, from_line: u32, before_line: u32) -> bool {
        self.comments
            .iter()
            .skip(self.next.get())
            .take_while(|comment| comment.line < before_line)
            .any(|comment| comment.line >= from_line)
    }

    /// Like `emit_before_line`, but bounded by a byte offset.
    fn emit_before_offset(
        &self,
        before: usize,
        depth: usize,
        config: &FormatConfig,
        out: &mut String,
    ) {
        if self.suspended.get() > 0 {
            return;
        }
        while let Some(c) = self.comments.get(self.next.get()) {
            if c.start >= before {
                break;
            }
            out.push_str(&indent(depth, config));
            out.push_str(&c.text);
            out.push('\n');
            self.next.set(self.next.get() + 1);
        }
    }

    /// Appends a trailing comment claimed for `on_line` to the line `out`
    /// just finished (before its newline), if there is one.
    fn append_trailing(&self, on_line: u32, out: &mut String) {
        if !out.ends_with('\n') {
            return;
        }
        if let Some(trailing) = self.take_trailing(on_line) {
            out.pop();
            out.push(' ');
            out.push_str(&trailing);
            out.push('\n');
        }
    }

    /// Consume a trailing comment on the given source line (if any), returning its text.
    fn take_trailing(&self, on_line: u32) -> Option<String> {
        if self.suspended.get() > 0 {
            return None;
        }
        let c = self.comments.get(self.next.get())?;
        if c.is_trailing && c.line == on_line {
            self.next.set(self.next.get() + 1);
            return Some(c.text.clone());
        }
        None
    }
}

/// The source line a top-level item's last token sits on — where a trailing
/// comment after the item lives.
fn item_end_line(item: &TopLevelItem) -> u32 {
    match item {
        TopLevelItem::Section(d) => d.end_line,
        TopLevelItem::Impl(d) => d.end_line,
        TopLevelItem::Function(d) => d.body.span.line,
        TopLevelItem::Task(d) => d.closing_span.line,
        TopLevelItem::Enum(d) => d.end_line,
        TopLevelItem::Type(d) if d.end_line > 0 => d.end_line,
        _ => item_span_line(item),
    }
}

fn item_span_line(item: &TopLevelItem) -> u32 {
    match item {
        TopLevelItem::Import(d) => d.span.line,
        TopLevelItem::Var(d) => d.span.line,
        TopLevelItem::Dynamic(d) => d.span.line,
        TopLevelItem::Section(d) => d.span.line,
        TopLevelItem::Impl(d) => d.span.line,
        TopLevelItem::Function(d) => d.span.line,
        TopLevelItem::SchemaSection(d) => d.span.line,
        TopLevelItem::Type(d) => d.span.line,
        TopLevelItem::Enum(d) => d.span.line,
        TopLevelItem::FunctionGroup(d) => d.span.line,
        TopLevelItem::SchemaFrom(d) => d.span.line,
        TopLevelItem::Task(d) => d.span.line,
        TopLevelItem::Statement(statement) => match statement {
            Statement::LocalVar(declaration) => declaration.span.line,
            Statement::Assignment { span, .. } | Statement::FieldAssignment { span, .. } => {
                span.line
            }
            Statement::Expression(_, span)
            | Statement::Return(_, span)
            | Statement::Break(span)
            | Statement::Continue(span) => span.line,
            Statement::If(statement) => statement.span.line,
            Statement::For(statement) => statement.span.line,
            Statement::Try(statement) => statement.span.line,
        },
    }
}

fn format_top_level_item(
    item: &TopLevelItem,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
) {
    match item {
        TopLevelItem::Import(imp) => format_import_decl(imp, out),

        TopLevelItem::Var(vd) => {
            if vd.exported {
                out.push_str("export ");
            }
            out.push_str("var ");
            if vd.mutable {
                out.push_str("mut ");
            }
            out.push_str(&vd.name);
            if vd.optional {
                out.push('?');
            }
            out.push_str(": ");
            out.push_str(&format_type(&vd.ty));
            if let Some(val) = &vd.value {
                out.push_str(" = ");
                format_expr(val, 0, 0, config, out);
            }
            out.push_str(";\n");
        }

        TopLevelItem::Dynamic(dd) => {
            out.push_str("dynamic var ");
            out.push_str(&dd.name);
            if dd.optional {
                out.push('?');
            }
            if let Some(val) = &dd.value {
                out.push_str(" = ");
                format_expr(val, 0, 0, config, out);
            }
            out.push_str(";\n");
        }

        TopLevelItem::Section(sd) => {
            if sd.exported {
                out.push_str("export ");
            }
            if sd.private {
                out.push_str("private ");
            }
            if sd.path.len() == 1 {
                out.push_str("struct ");
                out.push_str(&sd.path.join("."));
            } else {
                out.push('[');
                out.push_str(&sd.path.join("."));
                out.push(']');
            }
            if let Some(binding) = &sd.type_binding {
                out.push_str(if sd.path.len() == 1 { ": " } else { " -> " });
                out.push_str(&format_type(&binding.ty));
                out.push_str(" {\n");
            } else {
                out.push_str(if sd.path.len() == 1 { " {\n" } else { "{\n" });
            }
            format_section_items(&sd.items, 1, config, cx, out, sd.path.len() == 1);
            cx.emit_before_line(sd.end_line, 1, config, out);
            out.push_str("};\n");
        }

        TopLevelItem::Impl(implementation) => {
            out.push_str("impl");
            format_type_parameters(&implementation.type_parameters, out);
            out.push(' ');
            out.push_str(&format_type(&implementation.target));
            out.push_str(" {\n");
            for method in &implementation.methods {
                let fd = &method.function;
                if fd.is_private {
                    out.push_str(&indent(1, config));
                    out.push_str("private ");
                } else {
                    out.push_str(&indent(1, config));
                }
                if fd.is_async {
                    out.push_str("async ");
                }
                out.push_str("function ");
                out.push_str(&fd.name);
                format_type_parameters(&fd.type_parameters, out);
                out.push('(');
                for (index, parameter) in fd.params.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    if index == 0 {
                        if let Some(receiver) = &method.receiver {
                            if receiver.mutable {
                                out.push_str("mut ");
                            }
                            out.push_str("self");
                            continue;
                        }
                    }
                    out.push_str(&parameter.name);
                    out.push_str(": ");
                    out.push_str(&format_type(&parameter.ty));
                    if let Some(default) = &parameter.default {
                        out.push_str(" = ");
                        format_expr(default, 0, 1, config, out);
                    }
                }
                out.push_str(") -> ");
                out.push_str(&format_type(&fd.ret));
                out.push_str(" {\n");
                format_func_stmts(&fd.body.stmts, 2, config, cx, out, false);
                out.push_str(&indent(1, config));
                out.push_str("};\n");
            }
            cx.emit_before_line(implementation.end_line, 1, config, out);
            out.push_str("};\n");
        }

        TopLevelItem::Function(fd) => {
            if fd.is_private {
                out.push_str("private ");
            }
            if fd.is_async {
                out.push_str("async ");
            }
            out.push_str("function ");
            out.push_str(&fd.name);
            format_type_parameters(&fd.type_parameters, out);
            out.push('(');
            for (i, p) in fd.params.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&p.name);
                out.push_str(": ");
                out.push_str(&format_type(&p.ty));
                if let Some(default) = &p.default {
                    out.push_str(" = ");
                    format_expr(default, 0, 0, config, out);
                }
            }
            out.push_str(") -> ");
            out.push_str(&format_type(&fd.ret));
            out.push_str(" {\n");
            format_func_stmts(&fd.body.stmts, 1, config, cx, out, false);
            cx.emit_before_line(fd.body.span.line, 1, config, out);
            out.push_str("};\n");
        }

        TopLevelItem::SchemaSection(sd) => {
            out.push_str("Schema");
            if sd.marker.optional {
                out.push('?');
            }
            out.push_str(" [");
            out.push_str(&sd.name);
            out.push_str("]{\n");
            for field in &sd.fields {
                format_schema_field(field, 1, config, out);
            }
            out.push_str("};\n");
        }

        TopLevelItem::Type(td) => {
            if td.exported {
                out.push_str("export ");
            }
            out.push_str("type ");
            out.push_str(&td.name);
            format_type_parameters(&td.type_parameters, out);
            out.push_str(" {\n");
            for field in &td.fields {
                format_type_field(field, 1, config, out);
            }
            cx.emit_before_line(td.end_line, 1, config, out);
            out.push_str("};\n");
        }

        TopLevelItem::Enum(ed) => {
            if ed.exported {
                out.push_str("export ");
            }
            out.push_str("enum ");
            out.push_str(&ed.name);
            out.push_str(" {\n");
            for (i, v) in ed.variants.iter().enumerate() {
                let line = ed.variant_lines.get(i).copied().unwrap_or(0);
                cx.emit_before_line(line, 1, config, out);
                out.push_str("    ");
                out.push_str(v);
                if i + 1 < ed.variants.len() {
                    out.push(',');
                }
                out.push('\n');
                cx.append_trailing(line, out);
            }
            cx.emit_before_line(ed.end_line, 1, config, out);
            out.push_str("};\n");
        }

        TopLevelItem::FunctionGroup(gd) => {
            if gd.is_private {
                out.push_str("private ");
            }
            out.push_str("functionGroup ");
            out.push_str(&gd.name);
            out.push_str(" {\n");
            for f in &gd.functions {
                out.push_str("    ");
                if f.is_private {
                    out.push_str("private ");
                }
                if f.is_async {
                    out.push_str("async ");
                }
                out.push_str("function ");
                out.push_str(&f.name);
                format_type_parameters(&f.type_parameters, out);
                out.push('(');
                for (i, p) in f.params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&p.name);
                    out.push_str(": ");
                    out.push_str(&format_type(&p.ty));
                    if let Some(default) = &p.default {
                        out.push_str(" = ");
                        format_expr(default, 0, 0, config, out);
                    }
                }
                out.push_str(") -> ");
                out.push_str(&format_type(&f.ret));
                out.push_str(" {\n");
                format_func_stmts(&f.body.stmts, 2, config, cx, out, false);
                cx.emit_before_line(f.body.span.line, 2, config, out);
                out.push_str("    }\n");
            }
            out.push_str("};\n");
        }

        TopLevelItem::SchemaFrom(sf) => {
            out.push_str("SchemaFrom");
            if sf.marker.optional {
                out.push('?');
            }
            out.push_str(" [");
            out.push_str(&sf.name);
            out.push_str(", ");
            out.push_str(&sf.source_type);
            out.push_str("];\n");
        }

        TopLevelItem::Task(td) => format_task_decl_cx(td, config, cx, out),

        TopLevelItem::Statement(statement) => {
            format_func_stmt(statement, 0, config, cx, out, false)
        }
    }
}

/// The source line the parser recorded for metadata field `name` (see
/// `TaskDecl::field_spans`) — `None` if the field wasn't written at all.
fn task_field_line(td: &TaskDecl, name: &str) -> Option<u32> {
    td.field_spans
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, span)| span.line)
}

/// Flushes any standalone comments before `name`'s source line (if the
/// field is present at all — an absent field never had a line to anchor
/// on) and appends a same-line trailing comment, if any, after `body` runs.
fn with_task_field_comments(
    td: &TaskDecl,
    name: &str,
    cx: &mut CommentCursor,
    config: &FormatConfig,
    out: &mut String,
    body: impl FnOnce(&mut String),
) {
    let Some(line) = task_field_line(td, name) else {
        return;
    };
    cx.emit_before_line(line, 1, config, out);
    body(out);
    if let Some(trailing) = cx.take_trailing(line) {
        if out.ends_with('\n') {
            out.pop();
            out.push(' ');
            out.push_str(&trailing);
            out.push('\n');
        }
    }
}

fn format_task_decl_cx(
    td: &TaskDecl,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
) {
    out.push_str("task ");
    out.push_str(&td.name);
    if !td.params.is_empty() {
        out.push('(');
        for (i, p) in td.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            if p.variadic {
                out.push('*');
            }
            out.push_str(&p.name);
            out.push_str(": ");
            out.push_str(&format_type(&p.ty));
            if let Some(default) = &p.default {
                out.push_str(" = ");
                format_expr(default, 0, 0, config, out);
            }
        }
        out.push(')');
    }
    out.push_str(" {\n");

    let body_indent = indent(1, config);

    if let Some(desc) = &td.description {
        with_task_field_comments(td, "description", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("description: ");
            format_expr(desc, 0, 1, config, out);
            out.push_str(";\n");
        });
    }
    if let Some(default) = &td.default {
        with_task_field_comments(td, "default", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("default: ");
            format_expr(default, 0, 1, config, out);
            out.push_str(";\n");
        });
    }
    if let Some(quiet) = &td.quiet {
        with_task_field_comments(td, "quiet", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("quiet: ");
            format_expr(quiet, 0, 1, config, out);
            out.push_str(";\n");
        });
    }
    if let Some(private) = &td.private {
        with_task_field_comments(td, "private", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("private: ");
            format_expr(private, 0, 1, config, out);
            out.push_str(";\n");
        });
    }
    if let Some(group) = &td.group {
        with_task_field_comments(td, "group", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("group: ");
            format_expr(group, 0, 1, config, out);
            out.push_str(";\n");
        });
    }
    if let Some(confirm) = &td.confirm {
        with_task_field_comments(td, "confirm", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("confirm: ");
            format_expr(confirm, 0, 1, config, out);
            out.push_str(";\n");
        });
    }
    if !td.depends_on.is_empty() {
        with_task_field_comments(td, "dependsOn", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("dependsOn: [");
            for (i, dep) in td.depends_on.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&dep.name);
            }
            out.push_str("];\n");
        });
    }
    if let Some(cwd) = &td.cwd {
        with_task_field_comments(td, "cwd", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("cwd: ");
            format_expr(cwd, 0, 1, config, out);
            out.push_str(";\n");
        });
    }
    if !td.env.is_empty() {
        with_task_field_comments(td, "env", cx, config, out, |out| {
            out.push_str(&body_indent);
            out.push_str("env: {\n");
            let env_indent = indent(2, config);
            for (key, value) in &td.env {
                out.push_str(&env_indent);
                out.push_str(key);
                out.push_str(": ");
                format_expr(value, 0, 2, config, out);
                out.push_str(";\n");
            }
            out.push_str(&body_indent);
            out.push_str("};\n");
        });
    }

    for block in &td.run_blocks {
        cx.emit_before_line(block.span.line, 1, config, out);
        out.push_str(&body_indent);
        out.push_str("run");
        if block.shell == RunShell::Bash {
            out.push_str(" bash");
        }
        if let Some(os) = &block.os {
            out.push(' ');
            out.push_str(os);
        }
        match &block.body {
            RunBody::Bash(commands) => {
                out.push_str(" {\n");
                let run_indent = indent(2, config);
                for cmd in commands {
                    out.push_str(&run_indent);
                    for part in &cmd.parts {
                        match part {
                            ShellTemplatePart::Literal(s) => out.push_str(s),
                            ShellTemplatePart::Expr(e) => {
                                out.push_str("${");
                                format_expr(e, 0, 2, config, out);
                                out.push('}');
                            }
                        }
                    }
                    if cmd.is_shebang {
                        out.push('\n');
                    } else {
                        out.push_str(";\n");
                    }
                }
                out.push_str(&body_indent);
                out.push_str("};\n");
            }
            RunBody::Native(shell) => {
                out.push(' ');
                format_native_run_body_cx(shell, 1, config, cx, out);
                out.push_str(";\n");
            }
        }
    }

    // Anything left standalone before the closing `}` (e.g. a comment after
    // the last run block, or a commented-out field with no live counterpart
    // at all) stays inside the task rather than leaking into whatever comes
    // after it.
    cx.emit_before_line(td.closing_span.line, 1, config, out);

    out.push_str("};\n");
}

fn format_import_decl(imp: &ImportDecl, out: &mut String) {
    match &imp.kind {
        ImportKind::Schema => {
            out.push_str("import schema \"");
            out.push_str(&escape_string_content(&imp.path));
            out.push_str("\";\n");
        }
        ImportKind::Aliased(alias) => {
            out.push_str(if imp.package {
                "import pkg \""
            } else {
                "import \""
            });
            out.push_str(&escape_string_content(&imp.path));
            out.push('"');
            if let Some(alias) = alias {
                out.push_str(" as ");
                out.push_str(alias);
            }
            out.push_str(";\n");
        }
        ImportKind::Selective(items) => {
            format_selective_import(imp.package, false, items, &imp.path, out)
        }
        ImportKind::TypeSelective(items) => {
            format_selective_import(imp.package, true, items, &imp.path, out)
        }
    }
}

fn format_import_item(item: &ImportItem, out: &mut String) {
    out.push_str(&item.name);
    if let Some(alias) = &item.alias {
        out.push_str(" as ");
        out.push_str(alias);
    }
}

fn format_selective_import(
    package: bool,
    type_only: bool,
    items: &[ImportItem],
    path: &str,
    out: &mut String,
) {
    let prefix = match (package, type_only) {
        (true, true) => "import pkg type ",
        (true, false) => "import pkg ",
        (false, true) => "import type ",
        (false, false) => "import ",
    };

    let mut flat = String::new();
    flat.push_str(prefix);
    format_import_items(items, &mut flat);
    flat.push_str(" from \"");
    flat.push_str(&escape_string_content(path));
    flat.push_str("\";");

    // A selective import is much easier to scan vertically once it has a
    // handful of names, even when the raw character count would technically
    // fit on one line. Short imports remain compact.
    let wrap = items.len() >= 4 || current_column(out) + flat.chars().count() > MAX_LINE_WIDTH;
    if !wrap {
        out.push_str(&flat);
        out.push('\n');
        return;
    }

    out.push_str(prefix);
    out.push_str("{\n");
    for (index, item) in items.iter().enumerate() {
        out.push_str("    ");
        format_import_item(item, out);
        if index + 1 < items.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("} from \"");
    out.push_str(&escape_string_content(path));
    out.push_str("\";\n");
}

fn format_import_items(items: &[ImportItem], out: &mut String) {
    out.push_str("{ ");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        format_import_item(item, out);
    }
    out.push_str(" }");
}

fn format_section_items(
    items: &[SectionItem],
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
    canonical: bool,
) {
    for item in items {
        match item {
            SectionItem::Field(fd) => format_field_decl(fd, depth, config, cx, out, canonical),
            SectionItem::Spread(ss) => {
                cx.emit_before_line(ss.span.line, depth, config, out);
                let ind = indent(depth, config);
                out.push_str(&ind);
                out.push_str("...");
                format_expr(&ss.expr, 0, depth, config, out);
                out.push_str(";\n");
                cx.append_trailing(ss.span.line, out);
            }
        }
    }
}

fn format_type(ty: &SparType) -> String {
    match ty {
        SparType::Str => "str".to_string(),
        SparType::Int => "int".to_string(),
        SparType::Float => "float".to_string(),
        SparType::Bool => "bool".to_string(),
        SparType::Section => "section".to_string(),
        SparType::Void => "void".to_string(),
        SparType::Shell => "shell".to_string(),
        SparType::Error => "error".to_string(),
        SparType::List(inner) => format!("List<{}>", format_type(inner)),
        SparType::Named(name) => name.clone(),
        SparType::TypeParameter(name) => name.clone(),
        SparType::Applied { name, arguments } => format!(
            "{}<{}>",
            name,
            arguments
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        SparType::Function {
            params,
            return_type,
        } => format!(
            "fn({}) -> {}",
            params
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", "),
            format_type(return_type)
        ),
    }
}

fn format_type_parameters(parameters: &[TypeParameter], out: &mut String) {
    if parameters.is_empty() {
        return;
    }
    out.push('<');
    out.push_str(
        &parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push('>');
}

fn binop_symbol(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Fallback => "??",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::LtEq => "<=",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

// Higher number = tighter binding. Used to decide when parens are needed.
fn binop_prec(op: &BinOp) -> u8 {
    match op {
        BinOp::Fallback => 1,
        BinOp::Or => 2,
        BinOp::And => 3,
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => 4,
        BinOp::Add | BinOp::Sub => 5,
        BinOp::Mul | BinOp::Div => 6,
    }
}

fn escape_string_content(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => {
                out.push('\\');
                out.push('"');
            }
            '\\' => {
                out.push('\\');
                out.push('\\');
            }
            '\n' => {
                out.push('\\');
                out.push('n');
            }
            '\t' => {
                out.push('\\');
                out.push('t');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Real line-width budget, measured as "already-written column + candidate
/// length" — NOT the candidate's own length in isolation. A short object
/// can still need wrapping once it's sitting after `        ports: [Port]
/// = ` on its real line; checking the candidate alone misses that
/// entirely. Set a couple of columns under the nominal 100-col target
/// (rustfmt's default) rather than exactly at it: whatever follows the
/// candidate on the same line — a field's `;`, a list item's `,` — isn't
/// part of the candidate string `fits_inline` measures, so budgeting to
/// the exact limit lets real lines land one or two columns over it.
const MAX_LINE_WIDTH: usize = 98;

/// How many columns into the current line `out` already is — the length
/// of everything written since the last `\n` (or all of `out`, if this is
/// still the first line).
fn current_column(out: &str) -> usize {
    match out.rfind('\n') {
        Some(i) => out[i + 1..].chars().count(),
        None => out.chars().count(),
    }
}

/// True if appending a candidate one-line rendering to `out` right now
/// both fits the real line-width budget and didn't already contain a
/// forced break — a child container that itself exceeded the budget
/// renders multi-line internally, and that embedded `\n` must propagate
/// outward: a parent can never stay on one line while wrapping a child
/// that didn't.
fn fits_inline(out: &str, candidate: &str) -> bool {
    !candidate.contains('\n') && current_column(out) + candidate.chars().count() <= MAX_LINE_WIDTH
}

fn object_prefers_multiline(items: &[SectionItem]) -> bool {
    if items.len() >= 3 {
        return true;
    }

    items.iter().any(|item| match item {
        SectionItem::Spread(_) => false,
        SectionItem::Field(field) => match &field.value {
            Some(FieldValue::Nested(_)) => true,
            Some(FieldValue::Expr(Expr::Object(_, _))) => true,
            Some(FieldValue::Expr(Expr::List(_, _))) => items.len() >= 2,
            _ => false,
        },
    })
}

fn list_prefers_multiline(items: &[Expr]) -> bool {
    items.iter().any(|item| matches!(item, Expr::Object(_, _)))
}

fn flatten_structured_pipe<'a>(expr: &'a Expr, stages: &mut Vec<&'a Expr>) -> &'a Expr {
    if let Expr::StructuredPipe { input, stage, .. } = expr {
        let base = flatten_structured_pipe(input, stages);
        stages.push(stage);
        base
    } else {
        expr
    }
}

pub(crate) fn format_expr(
    expr: &Expr,
    parent_prec: u8,
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    match expr {
        Expr::Object(items, object_span) => {
            let has_comments = config
                .comments
                .has_comment_in_offsets(object_span.start, object_span.end);
            let mut flat = String::from("{ ");
            let speculative = config.comments.suspend();
            for item in items {
                format_object_item_flat(item, depth + 1, config, &mut flat);
            }
            flat.push('}');
            drop(speculative);

            if !has_comments && !object_prefers_multiline(items) && fits_inline(out, &flat) {
                out.push_str(&flat);
            } else {
                out.push_str("{\n");
                let field_indent = indent(depth + 1, config);
                for item in items {
                    let (item_line, item_end_line) = section_item_lines(item);
                    config
                        .comments
                        .emit_before_line(item_line, depth + 1, config, out);
                    out.push_str(&field_indent);
                    format_object_item_wrapped(item, depth + 1, config, out);
                    out.push('\n');
                    config.comments.append_trailing(item_end_line, out);
                }
                config
                    .comments
                    .emit_before_offset(object_span.end, depth + 1, config, out);
                out.push_str(&indent(depth, config));
                out.push('}');
            }
        }
        Expr::Literal(lit) => match lit {
            Literal::Int(n) => out.push_str(&n.to_string()),
            Literal::Float(f) => {
                let s = format!("{}", f);
                // ensure at least one decimal point for round floats
                if s.contains('.') {
                    out.push_str(&s);
                } else {
                    out.push_str(&s);
                    out.push_str(".0");
                }
            }
            Literal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        },

        Expr::String(is) => {
            out.push('"');
            for part in &is.parts {
                match part {
                    StringPart::Literal(s) => out.push_str(&escape_string_content(s)),
                    StringPart::Expr(e) => {
                        out.push_str("${");
                        format_expr(e, 0, depth, config, out);
                        out.push('}');
                    }
                }
            }
            out.push('"');
        }

        Expr::NamespaceRef(nr) => {
            out.push_str(&nr.segments.join("::"));
        }

        Expr::FnCall(fc) => {
            out.push_str(&fc.name);
            out.push('(');
            for (i, arg) in fc.args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                format_expr(arg, 0, depth, config, out);
            }
            out.push(')');
        }

        Expr::Call {
            name,
            type_arguments,
            args,
            ..
        } => {
            out.push_str(name);
            if !type_arguments.is_empty() {
                out.push('<');
                out.push_str(
                    &type_arguments
                        .iter()
                        .map(format_type)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                out.push('>');
            }
            out.push('(');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&arg.param_name);
                out.push_str(": ");
                format_expr(&arg.value, 0, depth, config, out);
            }
            out.push(')');
        }

        Expr::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            format_expr(receiver, 100, depth, config, out);
            out.push('.');
            out.push_str(method);
            out.push('(');
            for (index, argument) in args.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                format_expr(argument, 0, depth, config, out);
            }
            out.push(')');
        }

        Expr::StructuredPipe { .. } => {
            let prec = 0;
            let needs_parens = prec < parent_prec;
            if needs_parens {
                out.push('(');
            }
            let mut stages = Vec::new();
            let base = flatten_structured_pipe(expr, &mut stages);
            format_expr(base, prec, depth, config, out);
            for stage in stages {
                out.push('\n');
                out.push_str(&indent(depth + 1, config));
                out.push_str("|> ");
                format_expr(stage, prec + 1, depth + 1, config, out);
            }
            if needs_parens {
                out.push(')');
            }
        }

        Expr::Closure {
            params,
            return_type,
            body,
            ..
        } => {
            out.push_str("fn(");
            for (index, param) in params.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&param.name);
                if let Some(ty) = &param.ty {
                    out.push_str(": ");
                    out.push_str(&format_type(ty));
                }
            }
            out.push(')');
            if let Some(ty) = return_type {
                out.push_str(" -> ");
                out.push_str(&format_type(ty));
            }
            match body {
                ClosureBody::Expr(value) => {
                    out.push_str(" => ");
                    format_expr(value, 0, depth, config, out);
                }
                ClosureBody::Block(body) => {
                    out.push_str(" {\n");
                    let mut cx = config.comments.clone();
                    format_func_stmts(&body.stmts, depth + 1, config, &mut cx, out, false);
                    out.push_str(&indent(depth, config));
                    out.push('}');
                }
            }
        }

        Expr::BinaryOp(b) => {
            let prec = binop_prec(&b.op);
            let needs_parens = prec < parent_prec;
            if needs_parens {
                out.push('(');
            }
            format_expr(&b.lhs, prec, depth, config, out);
            out.push(' ');
            out.push_str(binop_symbol(&b.op));
            out.push(' ');
            // Right side: use prec+1 so same-precedence right operand gets parens
            // (avoids ambiguity for non-associative ops like comparisons)
            format_expr(&b.rhs, prec + 1, depth, config, out);
            if needs_parens {
                out.push(')');
            }
        }

        Expr::Unary { op, operand, .. } => {
            match op {
                UnOp::Not => out.push('!'),
                UnOp::Neg => out.push('-'),
            }
            // Unary binds tighter than all binary ops (prec 7)
            format_expr(operand, 7, depth, config, out);
        }

        Expr::Await { value, .. } => {
            out.push_str("await ");
            format_expr(value, 7, depth, config, out);
        }

        Expr::List(items, _) => {
            let mut flat = String::from("[");
            let speculative = config.comments.suspend();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    flat.push_str(", ");
                }
                format_expr(item, 0, depth + 1, config, &mut flat);
            }
            flat.push(']');
            drop(speculative);

            if !list_prefers_multiline(items) && fits_inline(out, &flat) {
                out.push_str(&flat);
            } else {
                out.push_str("[\n");
                let item_indent = indent(depth + 1, config);
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&item_indent);
                    format_expr(item, 0, depth + 1, config, out);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&indent(depth, config));
                out.push(']');
            }
        }

        Expr::Grouped(inner, _) => {
            out.push('(');
            format_expr(inner, 0, depth, config, out);
            out.push(')');
        }

        Expr::Comprehension {
            var_name,
            source,
            body,
            ..
        } => {
            let mut flat = String::from("for ");
            let speculative = config.comments.suspend();
            flat.push_str(var_name);
            flat.push_str(" in ");
            format_expr(source, 0, depth, config, &mut flat);
            flat.push_str(" { ");
            format_expr(body, 0, depth + 1, config, &mut flat);
            flat.push_str(" }");
            drop(speculative);

            if fits_inline(out, &flat) {
                out.push_str(&flat);
            } else {
                out.push_str("for ");
                out.push_str(var_name);
                out.push_str(" in ");
                format_expr(source, 0, depth, config, out);
                out.push_str(" {\n");
                out.push_str(&indent(depth + 1, config));
                format_expr(body, 0, depth + 1, config, out);
                out.push('\n');
                out.push_str(&indent(depth, config));
                out.push('}');
            }
        }

        Expr::Index { source, index, .. } => {
            format_expr(source, 8, depth, config, out); // 8 = tightest: index always binds to immediate source
            out.push('[');
            format_expr(index, 0, depth, config, out);
            out.push(']');
        }

        Expr::FieldAccess { base, field, .. } => {
            format_expr(base, 8, depth, config, out); // 8 = tightest, same as Index's source
            out.push('.');
            out.push_str(field);
        }

        Expr::Shell(shell) => format_shell_expr(shell, false, depth, config, out),
        Expr::ExecShell(shell) => format_shell_expr(shell, true, depth, config, out),
        Expr::CommandSubstitution(shell) => {
            out.push_str("$(");
            format_shell_steps_inline(&shell.steps, depth, config, out);
            out.push(')');
        }
    }
}

fn format_shell_expr(
    shell: &ShellExpr,
    execute: bool,
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    if execute {
        out.push_str("exec ");
    }
    if let Some(foreign_shell) = &shell.foreign_shell {
        out.push_str("shell ");
        out.push_str(foreign_shell);
        out.push_str(" {");
        if let Some((_, ShellStep::Command(command))) = shell.steps.first() {
            if command.program.text == foreign_shell.as_str()
                && command
                    .args
                    .first()
                    .is_some_and(|argument| argument.text == "-c")
            {
                if let Some(source) = command.args.get(1) {
                    out.push_str(&source.text);
                }
            }
        }
        out.push('}');
        return;
    }
    if !execute {
        out.push_str("shell ");
    }
    format_native_run_body(shell, depth, config, out);
}

/// Prints a native shell block's `{ ... }` (no `shell`/`exec` prefix); shared
/// by `shell {}` values and `run { }` task bodies.
fn format_native_run_body(
    shell: &ShellExpr,
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    let mut cx = config.comments.clone();
    format_native_run_body_cx(shell, depth, config, &mut cx, out);
}

fn format_native_run_body_cx(
    shell: &ShellExpr,
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
) {
    out.push('{');
    if !shell.statements.is_empty() {
        out.push('\n');
        format_func_stmts(&shell.statements, depth + 1, config, cx, out, true);
        cx.emit_before_line(shell.end_line, depth + 1, config, out);
        out.push_str(&indent(depth, config));
        out.push('}');
        return;
    }
    if shell.steps.is_empty() {
        out.push('}');
        return;
    }
    out.push('\n');
    format_shell_steps(&shell.steps, depth + 1, config, out);
    cx.emit_before_line(shell.end_line, depth + 1, config, out);
    out.push_str(&indent(depth, config));
    out.push('}');
}

fn format_shell_steps_inline(
    steps: &[(ShellJoin, ShellStep)],
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    for (index, (join, step)) in steps.iter().enumerate() {
        if index > 0 {
            match join {
                ShellJoin::Always => out.push_str("; "),
                ShellJoin::OnSuccess => out.push_str(" && "),
                ShellJoin::OnFailure => out.push_str(" || "),
            }
        }
        match step {
            ShellStep::Command(command) => format_shell_command(command, out),
            ShellStep::Pipeline(commands) => {
                for (command_index, command) in commands.iter().enumerate() {
                    if command_index > 0 {
                        out.push_str(" | ");
                    }
                    format_shell_command(command, out);
                }
            }
            ShellStep::MixedPipeline(pipeline) => {
                format_mixed_shell_pipeline(pipeline, depth, config, out);
            }
        }
    }
}

fn shell_step_line(step: &ShellStep) -> u32 {
    match step {
        ShellStep::Command(command) => command.span.line,
        ShellStep::Pipeline(commands) => commands.first().map_or(0, |command| command.span.line),
        ShellStep::MixedPipeline(pipeline) => pipeline.span.line,
    }
}

fn format_shell_steps(
    steps: &[(ShellJoin, ShellStep)],
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    let cx = &config.comments;
    let mut previous_line = 0;
    for (index, (join, step)) in steps.iter().enumerate() {
        let line = shell_step_line(step);
        if index == 0 {
            cx.emit_before_line(line, depth, config, out);
            out.push_str(&indent(depth, config));
        } else {
            match join {
                ShellJoin::Always => {
                    out.push_str(";\n");
                    cx.append_trailing(previous_line, out);
                    cx.emit_before_line(line, depth, config, out);
                    out.push_str(&indent(depth, config));
                }
                ShellJoin::OnSuccess => out.push_str(" && "),
                ShellJoin::OnFailure => out.push_str(" || "),
            }
        }
        previous_line = line;
        match step {
            ShellStep::Command(command) => format_shell_command(command, out),
            ShellStep::Pipeline(commands) => {
                for (index, command) in commands.iter().enumerate() {
                    if index > 0 {
                        out.push_str(" | ");
                    }
                    format_shell_command(command, out);
                }
            }
            ShellStep::MixedPipeline(pipeline) => {
                format_mixed_shell_pipeline(pipeline, depth, config, out);
            }
        }
    }
    if !steps.is_empty() {
        out.push_str(";\n");
        cx.append_trailing(previous_line, out);
    }
}

fn format_mixed_shell_pipeline(
    pipeline: &ShellMixedPipeline,
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    for (index, command) in pipeline.input.iter().enumerate() {
        if index > 0 {
            out.push_str(" | ");
        }
        format_shell_command(command, out);
    }
    out.push_str(" | from ");
    if let Some(namespace) = pipeline.decoder.decoder.namespace {
        out.push_str(namespace.as_str());
        out.push_str("::");
    }
    out.push_str(&pipeline.decoder.decoder.name);
    if !pipeline.decoder.args.is_empty() {
        out.push('(');
        for (index, arg) in pipeline.decoder.args.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&arg.name);
            out.push_str(": ");
            format_expr(&arg.value, 0, depth, config, out);
        }
        out.push(')');
    }
    for stage in &pipeline.stages {
        out.push_str(" |> ");
        format_expr(stage, 0, depth, config, out);
    }
    if let Some(encoder) = &pipeline.encoder {
        out.push_str(" |> to ");
        out.push_str(&encoder.format);
    }
    for command in &pipeline.output {
        out.push_str(" | ");
        format_shell_command(command, out);
    }
    if let Some(redirect) = &pipeline.encoder_redirect {
        out.push_str(if redirect.mode == spar_command::RedirectMode::Append {
            " >> "
        } else {
            " > "
        });
        format_shell_word(&redirect.target.text, out);
    }
}

fn format_shell_command(command: &ShellCommandExpr, out: &mut String) {
    for environment in &command.environment {
        out.push_str(&environment.name);
        out.push('=');
        format_shell_word(&environment.value.text, out);
        out.push(' ');
    }
    format_shell_word(&command.program.text, out);
    for argument in &command.args {
        out.push(' ');
        format_shell_word(&argument.text, out);
    }
    if !command.redirections.is_empty() {
        for redirect in &command.redirections {
            out.push(' ');
            match &redirect.target {
                ShellFdRedirectTarget::File(file) => {
                    match (redirect.fd, &file.mode) {
                        (0, _) => out.push('<'),
                        (1, spar_command::RedirectMode::Truncate) => out.push('>'),
                        (1, spar_command::RedirectMode::Append) => out.push_str(">>"),
                        (fd, spar_command::RedirectMode::Truncate) => {
                            out.push_str(&format!("{fd}>"))
                        }
                        (fd, spar_command::RedirectMode::Append) => {
                            out.push_str(&format!("{fd}>>"))
                        }
                    }
                    out.push(' ');
                    format_shell_word(&file.target.text, out);
                }
                ShellFdRedirectTarget::Duplicate(target) => {
                    out.push_str(&format!("{}>&{target}", redirect.fd));
                }
            }
        }
    } else {
        if let Some(redirect) = &command.stdin {
            out.push_str(" < ");
            format_shell_word(&redirect.target.text, out);
        }
        if let Some(redirect) = &command.stdout {
            out.push_str(match redirect.mode {
                spar_command::RedirectMode::Truncate => " > ",
                spar_command::RedirectMode::Append => " >> ",
            });
            format_shell_word(&redirect.target.text, out);
        }
        if let Some(redirect) = &command.stderr {
            out.push_str(" 2> ");
            format_shell_word(&redirect.target.text, out);
        }
    }
    if command.background {
        out.push_str(" &");
    }
}

fn format_shell_word(word: &str, out: &mut String) {
    let needs_quotes = word.is_empty()
        || word.bytes().any(|byte| {
            byte.is_ascii_whitespace() || matches!(byte, b';' | b'|' | b'>' | b'{' | b'}' | b'"')
        });
    if !needs_quotes {
        out.push_str(word);
        return;
    }
    out.push('"');
    for character in word.chars() {
        if matches!(character, '\\' | '"') {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('"');
}

/// Renders one `{ ... }` object-literal field or spread, `"; "`-terminated,
/// shared verbatim by the inline and multi-line `Expr::Object` branches —
/// the multi-line branch strips the trailing space and adds its own `\n`.
fn format_object_item_flat(
    item: &SectionItem,
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    match item {
        SectionItem::Field(f) => {
            out.push_str(&f.name);
            if f.optional {
                out.push('?');
            }
            out.push_str(": ");
            if let Some(ty) = &f.ty {
                out.push_str(&format_type(ty));
                if f.value.is_some() {
                    out.push_str(" = ");
                }
            }
            match &f.value {
                Some(FieldValue::Expr(e)) => {
                    format_expr(e, 0, depth, config, out);
                }
                Some(FieldValue::Nested(items)) => {
                    out.push_str("{ ");
                    for item in items {
                        format_object_item_flat(item, depth + 1, config, out);
                    }
                    out.push('}');
                }
                None => {}
            }
            out.push_str("; ");
        }
        SectionItem::Spread(ss) => {
            out.push_str("...");
            format_expr(&ss.expr, 0, depth, config, out);
            out.push_str("; ");
        }
    }
}

/// Renders one object-literal item for the multi-line branch, without a
/// trailing space. Nested `{ ... }` values stay inline only when they are
/// small and fit the line budget; otherwise they expand one field per line.
/// First and last source line of an object/section item.
fn section_item_lines(item: &SectionItem) -> (u32, u32) {
    match item {
        SectionItem::Field(field) => (field.span.line, field.end_line),
        SectionItem::Spread(spread) => (spread.span.line, spread.span.line),
    }
}

fn format_object_item_wrapped(
    item: &SectionItem,
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) {
    if let SectionItem::Field(f) = item {
        if let (None, Some(FieldValue::Nested(nested))) = (&f.ty, &f.value) {
            let mut flat = String::new();
            let speculative = config.comments.suspend();
            format_object_item_flat(item, depth, config, &mut flat);
            drop(speculative);
            flat.pop(); // drop the trailing space after ';'
            let has_comments = config
                .comments
                .has_comment_in_lines(f.span.line, f.end_line);
            if !has_comments && !object_prefers_multiline(nested) && fits_inline(out, &flat) {
                out.push_str(&flat);
                return;
            }
            out.push_str(&f.name);
            if f.optional {
                out.push('?');
            }
            out.push_str(": {\n");
            for nested_item in nested {
                let (item_line, item_end_line) = section_item_lines(nested_item);
                config
                    .comments
                    .emit_before_line(item_line, depth + 1, config, out);
                out.push_str(&indent(depth + 1, config));
                format_object_item_wrapped(nested_item, depth + 1, config, out);
                out.push('\n');
                config.comments.append_trailing(item_end_line, out);
            }
            config
                .comments
                .emit_before_line(f.end_line, depth + 1, config, out);
            out.push_str(&indent(depth, config));
            out.push_str("};");
            return;
        }
    }
    format_object_item_flat(item, depth, config, out);
    out.pop(); // drop the flat variant's trailing space after ';'
}

fn indent(depth: usize, config: &FormatConfig) -> String {
    " ".repeat(depth * config.indent_width)
}

fn format_func_stmts(
    stmts: &[FuncStmt],
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
    shell_context: bool,
) {
    for stmt in stmts {
        format_func_stmt(stmt, depth, config, cx, out, shell_context);
    }
}

fn func_stmt_line(stmt: &FuncStmt) -> u32 {
    match stmt {
        FuncStmt::LocalVar(lv) => lv.span.line,
        FuncStmt::Assignment { span, .. }
        | FuncStmt::FieldAssignment { span, .. }
        | FuncStmt::Expression(_, span)
        | FuncStmt::Break(span)
        | FuncStmt::Continue(span)
        | FuncStmt::Return(_, span) => span.line,
        FuncStmt::Try(ts) => ts.span.line,
        FuncStmt::If(is) => is.span.line,
        FuncStmt::For(fs) => fs.span.line,
    }
}

fn format_func_stmt(
    stmt: &FuncStmt,
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
    shell_context: bool,
) {
    let line = func_stmt_line(stmt);
    cx.emit_before_line(line, depth, config, out);
    format_func_stmt_body(stmt, depth, config, cx, out, shell_context);
    // Block statements claim their trailing comment on the closing line;
    // simple ones on their own line.
    let trailing_line = match stmt {
        FuncStmt::Try(ts) => ts.end_line,
        FuncStmt::If(is) => is.end_line,
        FuncStmt::For(fs) => fs.end_line,
        _ => line,
    };
    cx.append_trailing(trailing_line, out);
}

fn format_func_stmt_body(
    stmt: &FuncStmt,
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
    shell_context: bool,
) {
    let ind = indent(depth, config);
    match stmt {
        FuncStmt::LocalVar(lv) => {
            out.push_str(&ind);
            out.push_str("var ");
            if lv.mutable {
                out.push_str("mut ");
            }
            out.push_str(&lv.name);
            if let Some(ty) = &lv.ty {
                out.push_str(": ");
                out.push_str(&format_type(ty));
            }
            out.push_str(" = ");
            format_expr(&lv.value, 0, depth, config, out);
            out.push_str(";\n");
        }

        FuncStmt::Assignment { name, value, .. } => {
            out.push_str(&ind);
            out.push_str(name);
            out.push_str(" = ");
            format_expr(value, 0, depth, config, out);
            out.push_str(";\n");
        }

        FuncStmt::FieldAssignment {
            base,
            fields,
            value,
            ..
        } => {
            out.push_str(&ind);
            out.push_str(base);
            for field in fields {
                out.push('.');
                out.push_str(field);
            }
            out.push_str(" = ");
            format_expr(value, 0, depth, config, out);
            out.push_str(";\n");
        }

        FuncStmt::Expression(expr, _) => {
            if shell_context {
                if let Expr::Shell(shell) = expr {
                    if shell.statements.is_empty() {
                        format_shell_steps(&shell.steps, depth, config, out);
                        return;
                    }
                }
            }
            out.push_str(&ind);
            format_expr(expr, 0, depth, config, out);
            out.push_str(";\n");
        }

        FuncStmt::Break(_) => {
            out.push_str(&ind);
            out.push_str("break;\n");
        }

        FuncStmt::Continue(_) => {
            out.push_str(&ind);
            out.push_str("continue;\n");
        }

        FuncStmt::Try(ts) => {
            out.push_str(&ind);
            out.push_str("try {\n");
            format_func_stmts(&ts.body, depth + 1, config, cx, out, shell_context);
            cx.emit_before_line(ts.catch_span.line, depth + 1, config, out);
            out.push_str(&ind);
            out.push_str("} catch");
            if let Some(name) = &ts.catch_name {
                out.push(' ');
                out.push_str(name);
            }
            out.push_str(" {\n");
            format_func_stmts(&ts.handler, depth + 1, config, cx, out, shell_context);
            cx.emit_before_line(ts.end_line, depth + 1, config, out);
            out.push_str(&ind);
            out.push_str("}\n");
        }

        FuncStmt::Return(rv, _) => {
            out.push_str(&ind);
            if matches!(rv, ReturnValue::Void) {
                out.push_str("return;\n");
                return;
            }
            out.push_str("return ");
            match rv {
                ReturnValue::Void => unreachable!("handled above"),
                ReturnValue::Expr(e) => {
                    format_expr(e, 0, depth, config, out);
                    out.push_str(";\n");
                }
                ReturnValue::SectionBlock(fields) => {
                    out.push_str("{\n");
                    for rf in fields {
                        cx.emit_before_line(rf.span.line, depth + 1, config, out);
                        out.push_str(&indent(depth + 1, config));
                        out.push_str(&rf.name);
                        out.push_str(": ");
                        if let Some(ty) = &rf.ty {
                            out.push_str(&format_type(ty));
                            out.push_str(" = ");
                        }
                        format_expr(&rf.value, 0, depth + 1, config, out);
                        out.push_str(";\n");
                        cx.append_trailing(rf.span.line, out);
                    }
                    out.push_str(&ind);
                    out.push_str("};\n");
                }
            }
        }

        FuncStmt::If(if_stmt) => {
            out.push_str(&ind);
            out.push_str("if ");
            format_expr(&if_stmt.condition, 0, depth, config, out);
            out.push_str(" {\n");
            format_func_stmts(
                &if_stmt.then_stmts,
                depth + 1,
                config,
                cx,
                out,
                shell_context,
            );
            cx.emit_before_line(if_stmt.then_end_line, depth + 1, config, out);
            if if_stmt.else_stmts.is_empty() {
                out.push_str(&ind);
                out.push_str("}\n");
            } else if let (true, Some(nested @ FuncStmt::If(_))) =
                (if_stmt.else_if, if_stmt.else_stmts.first())
            {
                // `else if`: the nested `if` continues the chain on this line.
                out.push_str(&ind);
                out.push_str("} else ");
                let start = out.len();
                format_func_stmt_body(nested, depth, config, cx, out, shell_context);
                out.replace_range(start..start + ind.len(), "");
            } else {
                out.push_str(&ind);
                out.push_str("} else {\n");
                format_func_stmts(
                    &if_stmt.else_stmts,
                    depth + 1,
                    config,
                    cx,
                    out,
                    shell_context,
                );
                cx.emit_before_line(if_stmt.end_line, depth + 1, config, out);
                out.push_str(&ind);
                out.push_str("}\n");
            }
        }

        FuncStmt::For(statement) => {
            out.push_str(&ind);
            out.push_str("for ");
            match &statement.binding {
                ForBinding::Value { name, .. } => out.push_str(name),
                ForBinding::Indexed {
                    index_name,
                    value_name,
                    ..
                } => {
                    out.push('(');
                    out.push_str(index_name);
                    out.push_str(", ");
                    out.push_str(value_name);
                    out.push(')');
                }
            }
            out.push_str(" in ");
            format_expr(&statement.iterable, 0, depth, config, out);
            out.push_str(" {\n");
            format_func_stmts(&statement.body, depth + 1, config, cx, out, shell_context);
            cx.emit_before_line(statement.end_line, depth + 1, config, out);
            out.push_str(&ind);
            out.push_str("}\n");
        }
    }
}

fn format_schema_field(field: &SchemaField, depth: usize, config: &FormatConfig, out: &mut String) {
    let indent = " ".repeat(depth * config.indent_width);
    out.push_str(&indent);
    out.push_str(&field.name);
    if field.optional {
        out.push('?');
    }
    out.push_str(": ");
    match &field.shape {
        SchemaFieldShape::Primitive(ty) => {
            out.push_str(&format_type(ty));
            out.push_str(";\n");
        }
        SchemaFieldShape::Section(nested) => {
            out.push_str("section = {\n");
            for nf in nested {
                format_schema_field(nf, depth + 1, config, out);
            }
            out.push_str(&indent);
            out.push_str("};\n");
        }
    }
}

fn format_type_field(field: &TypeField, depth: usize, config: &FormatConfig, out: &mut String) {
    config
        .comments
        .emit_before_line(field.span.line, depth, config, out);
    let is_section = format_type_field_body(field, depth, config, out);
    if !is_section {
        config.comments.append_trailing(field.span.line, out);
    }
}

/// Returns whether the field was a nested `section = { ... }`.
fn format_type_field_body(
    field: &TypeField,
    depth: usize,
    config: &FormatConfig,
    out: &mut String,
) -> bool {
    let indent = " ".repeat(depth * config.indent_width);
    out.push_str(&indent);
    out.push_str(&field.name);
    if field.optional {
        out.push('?');
    }
    out.push_str(": ");
    match &field.shape {
        TypeFieldShape::Primitive(ty) => {
            out.push_str(&format_type(ty));
        }
        TypeFieldShape::Named(name) => {
            out.push_str(name);
        }
        TypeFieldShape::TypeParameter(name) => {
            out.push_str(name);
        }
        TypeFieldShape::Applied { name, arguments } => {
            out.push_str(&format_type(&SparType::Applied {
                name: name.clone(),
                arguments: arguments.clone(),
            }));
        }
        TypeFieldShape::Section(nested) => {
            out.push_str("section = {\n");
            for nf in nested {
                format_type_field(nf, depth + 1, config, out);
            }
            out.push_str(&indent);
            out.push_str("};\n");
            return true;
        }
    }
    if let Some(default) = &field.default {
        out.push_str(" = ");
        format_expr(default, 0, depth, config, out);
    }
    out.push_str(";\n");
    false
}

fn format_field_decl(
    fd: &FieldDecl,
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
    canonical: bool,
) {
    cx.emit_before_line(fd.span.line, depth, config, out);
    format_field_decl_body(fd, depth, config, cx, out, canonical);
    cx.append_trailing(fd.end_line, out);
}

fn format_field_decl_body(
    fd: &FieldDecl,
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
    canonical: bool,
) {
    let ind = indent(depth, config);
    out.push_str(&ind);
    out.push_str(&fd.name);
    if fd.optional {
        out.push('?');
    }
    if fd.ty.is_some() || !canonical {
        out.push_str(": ");
    } else {
        out.push_str(" = ");
    }
    match &fd.ty {
        Some(ty) => {
            out.push_str(&format_type(ty));
            match &fd.value {
                None => {
                    out.push_str(";\n");
                }
                Some(FieldValue::Expr(e)) => {
                    out.push_str(" = ");
                    format_expr(e, 0, depth, config, out);
                    out.push_str(";\n");
                }
                Some(FieldValue::Nested(nested_items)) => {
                    out.push_str(" = {\n");
                    for ni in nested_items {
                        format_nested_section_item(ni, depth + 1, config, cx, out);
                    }
                    cx.emit_before_line(fd.end_line, depth + 1, config, out);
                    out.push_str(&ind);
                    out.push_str("};\n");
                }
            }
        }
        // Type omitted — inferred from the enclosing section's binding.
        // No `=`: the value follows the colon directly.
        None => match &fd.value {
            None => {
                out.push_str(";\n");
            } // shouldn't occur (parser requires a value here), but format gracefully
            Some(FieldValue::Expr(e)) => {
                format_expr(e, 0, depth, config, out);
                out.push_str(";\n");
            }
            Some(FieldValue::Nested(nested_items)) => {
                out.push_str("{\n");
                for ni in nested_items {
                    format_nested_section_item(ni, depth + 1, config, cx, out);
                }
                cx.emit_before_line(fd.end_line, depth + 1, config, out);
                out.push_str(&ind);
                out.push_str("};\n");
            }
        },
    }
}

/// A nested field body reuses `SectionItem` (a field or a `...Source;`
/// spread) — mirrors the top-level section-item printing, one indent
/// level deeper.
fn format_nested_section_item(
    item: &SectionItem,
    depth: usize,
    config: &FormatConfig,
    cx: &mut CommentCursor,
    out: &mut String,
) {
    format_section_items(std::slice::from_ref(item), depth, config, cx, out, false);
}

#[cfg(test)]
mod tests {

    #[test]
    fn formats_impl_methods_and_mutable_receivers_canonically() {
        let source = r#"struct User{name:str="Obi";active:bool=true;};impl User{function name(self)->str{return self.name;};private function normalized(self)->str{return self.name;};function deactivate(mut self)->void{self.active=false;};};"#;
        let formatted = format_source(source).expect("format should succeed");
        assert!(formatted.contains("impl User {"), "{formatted}");
        assert!(
            formatted.contains("function name(self) -> str"),
            "{formatted}"
        );
        assert!(
            formatted.contains("private function normalized(self) -> str"),
            "{formatted}"
        );
        assert!(
            formatted.contains("function deactivate(mut self) -> void"),
            "{formatted}"
        );
    }

    #[test]
    fn formats_callable_types_and_closures_canonically() {
        let source = "function main() -> int { var double: fn(int) -> int = fn(x:int)->int=>x*2; return 0; };";
        let formatted = format_source(source).expect("format should succeed");
        assert!(formatted.contains("fn(int) -> int"), "{formatted}");
        assert!(
            formatted.contains("fn(x: int) -> int => x * 2"),
            "{formatted}"
        );
    }

    #[test]
    fn formats_block_closure_vertically() {
        let source = "function main() -> int { var double: fn(int) -> int = fn(x:int)->int{return x*2;}; return 0; };";
        let formatted = format_source(source).expect("format should succeed");
        assert!(formatted.contains("fn(x: int) -> int {\n"), "{formatted}");
        assert!(formatted.contains("return x * 2;"), "{formatted}");
    }
    use super::*;

    fn fmt(src: &str) -> String {
        format_source(src).expect("format_source failed")
    }

    #[test]
    fn formats_simple_var() {
        assert_eq!(fmt("var x: int = 1;").trim(), "var x: int = 1;");
    }

    #[test]
    fn format_shell_block() {
        let source = "var x: shell = shell {\n    echo hi;\n};\n";
        assert_eq!(fmt(source).trim(), source.trim());
    }

    #[test]
    fn format_foreign_bash_block_preserves_explicit_boundary() {
        let source = r#"function main() -> shell {
    return shell bash {
        printf "%s" "$HOME"
    };
};
"#;
        let formatted = fmt(source);
        assert!(formatted.contains("shell bash {"), "{formatted}");
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn format_background_and_ordered_redirections_is_idempotent() {
        let source = r#"function main() -> shell {
    return shell {
        tool 2>&1 > out &;
        tool &>> log;
    };
};
"#;
        let formatted = fmt(source);
        assert!(formatted.contains("tool 2>&1 > out &;"), "{formatted}");
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn stderr_redirection_is_not_duplicated_by_formatting() {
        let source = r#"function main() -> shell {
    return shell {
        tool 2> errors.log;
    };
};
"#;
        let formatted = fmt(source);
        assert_eq!(formatted.matches("2> errors.log").count(), 1, "{formatted}");
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn command_substitution_preserves_every_control_chain_step() {
        let source = "var branch: str = $(git symbolic-ref --short HEAD || echo detached);\n";
        let formatted = fmt(source);

        assert!(
            formatted.contains("git symbolic-ref --short HEAD || echo detached"),
            "command substitution was truncated: {formatted}"
        );
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn format_mixed_shell_loop_preserves_native_command_syntax() {
        let source = r#"function main() -> shell {
    var files: [str] = ["one", "two"];
    return shell {
        var mut count: int = 0;
        for file in files {
            for other in files {
                echo "${file}:${other}";
                count += 1;
            }
        }
        echo "${count}";
    };
};
"#;

        let formatted = fmt(source);
        assert_eq!(fmt(&formatted), formatted);
        assert!(formatted.contains("                echo \"${file}:${other}\";"));
        assert!(!formatted.contains("            shell {"));
    }

    #[test]
    fn format_command_sugar_uses_the_canonical_shell_block() {
        let formatted = fmt("var x: shell = command echo hi;\n");
        assert_eq!(formatted.trim(), "var x: shell = shell {\n    echo hi;\n};");
    }

    #[test]
    fn format_pipeline_redirect_and_quoted_word() {
        let source =
            "var x: shell = shell {\n    cat \"my file.txt\" | grep error > test.log;\n};\n";
        assert_eq!(fmt(source).trim(), source.trim());
    }

    #[test]
    fn format_exec_shell() {
        let source = "function f() -> int {\n    var r: ExecResult = exec shell {\n        true;\n    };\n    return 0;\n};\n";
        let expected = "function f() -> int {\n    var r: ExecResult = exec {\n        true;\n    };\n    return 0;\n};\n";
        assert_eq!(fmt(source).trim(), expected.trim());
        assert_eq!(fmt(expected), expected);
    }

    #[test]
    fn format_async_await_is_idempotent() {
        let source =
            "async function main()->int{var pending:Promise<int> =value();return await pending;};";
        let once = format_source(source).unwrap();
        assert_eq!(format_source(&once).unwrap(), once);
        assert!(once.contains("async function main() -> int"));
        assert!(once.contains("return await pending;"));
    }

    #[test]
    fn format_inferred_exec_shell_local_preserves_omitted_type() {
        let src = "function f() -> int {\n    var r = exec shell {\n        true;\n    };\n    return r.exitCode;\n};\n";
        let expected = "function f() -> int {\n    var r = exec {\n        true;\n    };\n    return r.exitCode;\n};\n";
        assert_eq!(fmt(src).trim(), expected.trim());
    }

    #[test]
    fn normalizes_extra_spaces_around_colon_and_eq() {
        assert_eq!(fmt("var   x:int=1;").trim(), "var x: int = 1;");
    }

    #[test]
    fn format_object_literal_round_trips() {
        let src = "var x: Leaf = { name: \"a\"; size: 1; };\n";
        let formatted = fmt(src);
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
        assert!(formatted.contains("{ name:"), "got: {formatted}");
    }

    #[test]
    fn nested_object_values_are_never_dropped() {
        let src = concat!(
            "var config: section = { ",
            "python: { enabled: true; }; ",
            "kids: true; ",
            "};\n",
        );
        let formatted = fmt(src);

        assert!(
            formatted.contains("python: { enabled: true; };"),
            "got: {formatted}"
        );
        assert!(formatted.contains("kids: true;"), "got: {formatted}");
        assert_eq!(fmt(&formatted), formatted, "formatting must be idempotent");
    }

    #[test]
    fn shell_environment_prefix_is_never_dropped() {
        let src = concat!(
            "function main() -> shell {\n",
            "    return shell {\n",
            "        RUST_LOG=debug cargo run;\n",
            "    };\n",
            "};\n",
        );
        let formatted = fmt(src);

        assert!(
            formatted.contains("RUST_LOG=debug cargo run;"),
            "shell-local environment assignment was lost: {formatted}"
        );
        assert_eq!(fmt(&formatted), formatted, "formatting must be idempotent");
    }

    #[test]
    fn format_enum_decl_round_trips() {
        let src = "export enum Devices {\n    Ios,\n    Android,\n};\n";
        let formatted = fmt(src);
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
        assert!(formatted.contains("enum Devices"), "got: {formatted}");
    }

    #[test]
    fn format_function_group_decl_round_trips() {
        let src = "private functionGroup EdgeInsect {\n    function only() -> int {\n        return 1;\n    }\n};\n";
        let formatted = fmt(src);
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
        assert!(
            formatted.contains("functionGroup EdgeInsect"),
            "got: {formatted}"
        );
        assert!(formatted.contains("private "), "got: {formatted}");
    }

    #[test]
    fn long_comprehension_wraps_to_multiple_lines() {
        // Regression: a comprehension whose body is a many-field object
        // literal used to render as one unreadable ~350-char line.
        let src = concat!(
            "export var apiReplicas: [int] = for i in [0, 1, 2] { ",
            "{ name: replicaName(base: \"api\", index: i); ",
            "image: \"acme/api\"; tag: \"1.4.2\"; ",
            "restart: RestartPolicy::OnFailure; } };\n",
        );
        let formatted = fmt(src);
        assert!(
            formatted.lines().all(|l| l.chars().count() <= 100),
            "got: {formatted}"
        );
        assert!(
            formatted.contains("for i in [0, 1, 2] {\n"),
            "got: {formatted}"
        );
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
    }

    #[test]
    fn short_object_literal_stays_inline() {
        // A short object must NOT be forced onto multiple lines just
        // because SOME container expressions need wrapping elsewhere.
        let src = "var p: Port = { container: 8080; host: 8081; };\n";
        let formatted = fmt(src);
        assert_eq!(
            formatted,
            "var p: Port = { container: 8080; host: 8081; };\n"
        );
    }

    #[test]
    fn width_check_accounts_for_real_indentation_and_prefix() {
        // Regression: a nested object whose OWN flat rendering is short
        // (well under the width budget in isolation) still needs to wrap
        // once the real line is accounted for — deep indentation plus a
        // `field: [Type] = ` prefix, PLUS the trailing `;` the caller
        // appends right after the candidate (not part of what
        // `fits_inline` measures) can together push an individually-short
        // object past the line-width budget even though checking the
        // object's candidate string alone would say it fits.
        let src = concat!(
            "export var apiReplicas: [int] = for i in [0, 1, 2] {\n",
            "    {\n",
            "        ports: [{ container: 8080; host: Compute::replicaPort(basePort: 8081, index: i); }];\n",
            "    }\n",
            "};\n",
        );
        let formatted = fmt(src);
        assert!(
            formatted.lines().all(|l| l.chars().count() <= 100),
            "no line should exceed the width budget once real indentation is counted, got: {formatted}"
        );
        assert!(
            formatted.contains("ports: [\n"),
            "the ports list must wrap onto its own lines, got: {formatted}"
        );
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
    }

    #[test]
    fn long_list_literal_wraps_one_item_per_line() {
        let src = concat!(
            "export var names: [str] = [\"alpha-service\", \"beta-service\", ",
            "\"gamma-service\", \"delta-service\", \"epsilon-service\", \"zeta-service\"];\n",
        );
        let formatted = fmt(src);
        assert!(
            formatted.lines().all(|l| l.chars().count() <= 100),
            "got: {formatted}"
        );
        assert!(formatted.contains("[\n"), "got: {formatted}");
        assert!(
            formatted.contains("\"alpha-service\",\n"),
            "got: {formatted}"
        );
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
    }

    #[test]
    fn nested_wrapped_object_forces_parent_list_to_wrap_too() {
        // A break inside a child (the comprehension's object body) must
        // propagate outward — the containing list/field can't stay
        // single-line while its own content spans multiple lines.
        let src = concat!(
            "[Services]{\n",
            "    replicas: [int] = for i in [0, 1, 2] { ",
            "{ name: replicaName(base: \"api\", index: i); ",
            "image: \"acme/api\"; tag: \"1.4.2\"; ",
            "restart: RestartPolicy::OnFailure; } };\n",
            "};\n",
        );
        let formatted = fmt(src);
        assert!(
            formatted.lines().all(|l| l.chars().count() <= 100),
            "got: {formatted}"
        );
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
    }

    #[test]
    fn export_var_has_export_prefix() {
        assert_eq!(
            fmt("export var x: int = 1;").trim(),
            "export var x: int = 1;"
        );
    }

    #[test]
    fn optional_var_has_question_mark() {
        assert_eq!(fmt("var x?: int;").trim(), "var x?: int;");
    }

    #[test]
    fn import_without_alias() {
        assert_eq!(fmt(r#"import "a.spar";"#).trim(), r#"import "a.spar";"#);
    }

    #[test]
    fn import_with_alias() {
        assert_eq!(
            fmt(r#"import "a.spar" as a;"#).trim(),
            r#"import "a.spar" as a;"#
        );
    }

    #[test]
    fn section_with_one_field() {
        let out = fmt("[Server]{ port: int = 8080; };");
        assert!(out.contains("struct Server {"));
        assert!(out.contains("    port: int = 8080;"));
        assert!(out.contains("};"));
    }

    #[test]
    fn formats_canonical_struct_generic_type_list_and_catch() {
        let out = fmt(
            r#"type Pair<T, V> { left: T; right: V; }; struct Example: Pair<str, int> { left = "hello"; right = 42; }; function main() -> void { var values: List<str> = ["a", "b"]; try { return; } catch err { return; } };"#,
        );
        assert!(out.contains("type Pair<T, V> {"), "{out}");
        assert!(out.contains("struct Example: Pair<str, int> {"), "{out}");
        assert!(out.contains("left = \"hello\";"), "{out}");
        assert!(out.contains("List<str>"), "{out}");
        assert!(out.contains("catch err {"), "{out}");
        assert_eq!(fmt(&out), out);
    }

    #[test]
    fn private_section_has_private_prefix() {
        let out = fmt("private [S]{ x: int = 1; };");
        assert!(out.trim_start().starts_with("private struct S {"));
    }

    #[test]
    fn export_section_has_export_prefix() {
        let out = fmt("export [S]{ x: int = 1; };");
        assert!(out.trim_start().starts_with("export struct S {"));
    }

    #[test]
    fn nested_section_path_joined_with_dot() {
        // The parser only allows single-segment section names (dotted paths are rejected at
        // parse time). The formatter uses .join(".") on the Vec<String> path, which is correct
        // for the AST representation. This test verifies the bracket-wrapping with a valid input.
        let out = fmt("[A]{ x: int = 1; };");
        assert!(out.contains("struct A {"));
    }

    #[test]
    fn section_field_nested_value_indented() {
        let src = "[A]{ b: section = { c: int = 1; }; };";
        let out = fmt(src);
        assert!(out.contains("    b: section = {"));
        assert!(out.contains("        c: int = 1;"));
    }

    #[test]
    fn section_spread_uses_ellipsis() {
        let src = "[A]{ ...other; };";
        let out = fmt(src);
        assert!(out.contains("    ...other;"));
    }

    #[test]
    fn dynamic_var_with_list() {
        let src = "dynamic var tags = [1, 2, 3];";
        let out = fmt(src);
        assert!(out.trim().starts_with("dynamic var tags"));
        assert!(out.contains("= ["));
    }

    #[test]
    fn dynamic_var_optional_no_value() {
        let src = "dynamic var meta?;";
        assert_eq!(fmt(src).trim(), "dynamic var meta?;");
    }

    #[test]
    fn function_decl_formatted() {
        let src = "function f(x: int) -> int { return x; };";
        let out = fmt(src);
        assert!(out.contains("function f(x: int) -> int {"));
        assert!(out.contains("    return x;"));
        assert!(out.contains("};"));
        assert_eq!(fmt(&out), out);
    }

    #[test]
    fn function_parameter_default_round_trips() {
        let src = r#"function greet(name: str = "world") -> str { return name; };"#;
        let formatted = fmt(src);
        assert!(
            formatted.contains(r#"function greet(name: str = "world") -> str {"#),
            "{formatted}"
        );
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn private_function_has_private_prefix() {
        let src = "private function f(x: int) -> int { return x; };";
        let out = fmt(src);
        assert!(out.trim_start().starts_with("private function f"));
    }

    #[test]
    fn function_if_else_formatted() {
        let src = "function f(x: bool) -> int { if x { return 1; } else { return 0; } };";
        let out = fmt(src);
        assert!(out.contains("    if x {"));
        assert!(out.contains("        return 1;"));
        assert!(out.contains("    } else {"));
        assert!(out.contains("        return 0;"));
        assert!(out.contains("    }"));
    }

    #[test]
    fn function_for_loop_formatted() {
        let src = "function f(xs: [int]) -> int { for x in xs { return x; } return 0; };";
        let out = fmt(src);
        assert!(out.contains("    for x in xs {"));
        assert!(out.contains("        return x;"));
        assert!(out.contains("    }"));
    }

    #[test]
    fn function_section_return_formatted() {
        let src = "function f(x: int) -> section { return { v: int = x; }; };";
        let out = fmt(src);
        assert!(out.contains("    return {"));
        assert!(out.contains("        v: int = x;"));
        assert!(out.contains("    };"));
    }

    #[test]
    fn binop_add_formatted() {
        assert_eq!(fmt("var x: int = a + b;").trim(), "var x: int = a + b;");
    }

    #[test]
    fn binop_fallback_formatted() {
        assert_eq!(fmt("var x: int = a ?? b;").trim(), "var x: int = a ?? b;");
    }

    #[test]
    fn unary_not_formatted() {
        assert_eq!(fmt("var x: bool = !flag;").trim(), "var x: bool = !flag;");
    }

    #[test]
    fn unary_neg_formatted() {
        assert_eq!(fmt("var x: int = -1;").trim(), "var x: int = -1;");
    }

    #[test]
    fn grouped_expr_keeps_parens() {
        let src = "var x: int = (a + b);";
        assert_eq!(fmt(src).trim(), "var x: int = (a + b);");
    }

    #[test]
    fn list_literal_formatted() {
        assert_eq!(
            fmt("var xs: [int] = [1, 2, 3];").trim(),
            "var xs: List<int> = [1, 2, 3];"
        );
    }

    #[test]
    fn namespace_ref_formatted() {
        assert_eq!(fmt("var x: int = A::b::c;").trim(), "var x: int = A::b::c;");
    }

    #[test]
    fn fn_call_positional_formatted() {
        assert_eq!(
            fmt("var x: str = env(\"PORT\");").trim(),
            "var x: str = env(\"PORT\");"
        );
    }

    #[test]
    fn call_named_args_formatted() {
        assert_eq!(
            fmt("var x: str = greet(name: \"world\");").trim(),
            "var x: str = greet(name: \"world\");"
        );
    }

    #[test]
    fn generic_syntax_formats_canonically_and_idempotently() {
        let source = "type [Pair<T,U,>]{left:T;right:U;};function pair<T,U,>(left:T,right:U)->Pair<T,U>{return {left:left;right:right;};};";
        let once = fmt(source);
        assert!(once.contains("type Pair<T, U> {"), "{once}");
        assert!(
            once.contains("function pair<T, U>(left: T, right: U) -> Pair<T, U>"),
            "{once}"
        );
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn explicit_generic_call_round_trips() {
        let once = fmt("var value: int = identity<int>(value: 1);");
        assert!(once.contains("identity<int>(value: 1)"), "{once}");
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn comprehension_formatted() {
        let src = "var xs: [int] = for x in items { x };";
        let out = fmt(src);
        assert!(out.contains("for x in items { x }"));
    }

    #[test]
    fn index_expr_formatted() {
        assert_eq!(fmt("var x: int = xs[0];").trim(), "var x: int = xs[0];");
    }

    #[test]
    fn list_type_nested_formatted() {
        assert_eq!(
            fmt("var x: [[int]] = [];").trim(),
            "var x: List<List<int>> = [];"
        );
    }

    #[test]
    fn bool_literal_formatted() {
        assert_eq!(fmt("var x: bool = true;").trim(), "var x: bool = true;");
        assert_eq!(fmt("var y: bool = false;").trim(), "var y: bool = false;");
    }

    #[test]
    fn float_literal_formatted() {
        assert_eq!(fmt("var x: float = 3.14;").trim(), "var x: float = 3.14;");
    }

    #[test]
    fn formatting_is_idempotent() {
        let src = r#"
import "a.spar" as a;

export var name: str = "keel";

var opt?: int;

dynamic var tags = [1, 2, 3];

[Server]{
    host: str = "0.0.0.0";
    port: int = 8080;
    nested: section = {
        debug: bool = false;
    };
    ...a;
};

private [Meta]{
    version: int = 1;
};

function pick(flag: bool) -> int {
    var base: int = 9000;
    if flag {
        return base;
    } else {
        return 0;
    }
};
"#;
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "formatting must be idempotent");
    }

    #[test]
    fn formats_despite_unresolved_import() {
        // parse-only: import file doesn't exist, but formatting should still work
        let src = r#"import "does_not_exist.spar" as x; var a: int = 1;"#;
        assert!(format_source(src).is_ok());
    }

    #[test]
    fn returns_err_for_unparseable_source() {
        assert!(format_source("this is not valid keel {{{").is_err());
    }

    #[test]
    fn multiple_blank_lines_collapse_to_one() {
        let src = "var a: int = 1;\n\n\n\nvar b: int = 2;";
        let out = format_source(src).unwrap();
        assert!(
            !out.contains("\n\n\n"),
            "more than one consecutive blank line found"
        );
    }

    #[test]
    fn string_with_escapes_roundtrips() {
        // Quoted chars must survive format → parse → format unchanged.
        let src = r#"var x: str = "say \"hi\"";"#;
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "idempotency broken on escape sequences");
        assert!(
            once.contains(r#"\"hi\""#),
            "escaped quote must be re-escaped in output"
        );
    }

    #[test]
    fn string_with_newline_escape_roundtrips() {
        let src = "var x: str = \"line1\\nline2\";";
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice);
        assert!(once.contains("\\n"), "newline escape must be preserved");
    }

    #[test]
    fn import_path_with_special_chars_roundtrips() {
        // Import paths go through the same escape-decoding as strings.
        // A path stored with a literal backslash must be re-escaped on output.
        // We can't test literal '"' in import path because the parser rejects it,
        // but we can verify the escape_string_content function is called via a
        // direct format_program call on a hand-crafted AST.
        use crate::ast::*;
        let program = Program {
            is_schema_file: false,
            load_env: None,
            shebang: None,
            items: vec![TopLevelItem::Import(ImportDecl {
                path: "dir\\file.spar".to_string(), // stored with literal backslash
                package: false,
                kind: ImportKind::Aliased(Some("x".to_string())),
                span: crate::error::Span::dummy(),
            })],
        };
        let out = format_program(&program, &FormatConfig::default());
        assert!(
            out.contains(r#"import "dir\\file.spar""#),
            "backslash must be re-escaped: {}",
            out
        );
    }

    #[test]
    fn section_path_join_with_dot() {
        use crate::ast::*;
        // Hand-craft a SectionDecl with path = ["A", "B"] to verify .join(".")
        // (the parser rejects "[A.B]" in source, but the AST can represent it)
        let program = Program {
            is_schema_file: false,
            load_env: None,
            shebang: None,
            items: vec![TopLevelItem::Section(SectionDecl {
                exported: false,
                private: false,
                canonical: false,
                path: vec!["A".to_string(), "B".to_string()],
                items: vec![],
                type_binding: None,
                span: crate::error::Span::dummy(),
                end_line: 0,
            })],
        };
        let out = format_program(&program, &FormatConfig::default());
        assert!(
            out.contains("[A.B]{"),
            "multi-segment path must be joined with '.'"
        );
    }

    #[test]
    fn formats_schema_file_with_pragma() {
        let src = "@SchemaFile\nSchema [X]{\n    a: int;\n};\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.starts_with("@SchemaFile\n"),
            "must start with @SchemaFile pragma: {}",
            formatted
        );
        assert!(
            formatted.contains("Schema [X]{"),
            "must contain schema section header: {}",
            formatted
        );
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn formats_spread_inside_nested_field_body() {
        let src = concat!(
            "[Postgres] -> PostgresType {\n",
            "    image: \"postgres:16\";\n",
            "    environment: { ...ProductionEnvironment; };\n",
            "};\n",
        );
        let once = format_source(src).expect("format");
        assert!(once.contains("...ProductionEnvironment;"), "got: {once}");
        let twice = format_source(&once).expect("format again");
        assert_eq!(once, twice, "formatting must be idempotent");
    }

    #[test]
    fn formats_selective_imports() {
        let src = concat!(
            "import { A, B as C } from \"shared.spar\";\n",
            "import type { PostgresType } from \"types.spar\";\n",
        );
        let once = format_source(src).expect("format");
        assert!(
            once.contains("import { A, B as C } from \"shared.spar\";"),
            "got: {once}"
        );
        assert!(
            once.contains("import type { PostgresType } from \"types.spar\";"),
            "got: {once}"
        );
        let twice = format_source(&once).expect("format again");
        assert_eq!(once, twice, "formatting must be idempotent");
    }

    #[test]
    fn formats_sparsh_config_with_readable_multiline_layout() {
        let src = r#"// Sparsh startup/config entry point.
import type { SparshAlias, SparshEnvironmentVariable, SparshPrompt, SparshHistory, SparshCompletion } from "sparsh-types.spar";
import { greet, build, showFile, rsBinInstall, zipOccLang } from "functions.spar";

struct Config {
    aliases: List<SparshAlias> = [
        { name: "gs"; command: ["git", "status"]; },
        { name: "ls"; command: ["eza", "-la", "--icons", "--group-directories-first"]; }
    ];
    environment: List<SparshEnvironmentVariable> = [
        { name: "EDITOR"; value: "nvim"; },
        { name: "PATH"; prepend: ["$HOME/.local/bin", "$HOME/.cargo/bin"]; append: ["$HOME/.pub-cache/bin"]; }
    ];
    prompt: SparshPrompt = {
        showStatus: true;
        showDuration: true;
        durationThresholdMs: 2000;
        path: { enabled: true; parentLength: 64; maxLastLength: 96; maxWidth: 512; };
        git: { enabled: true; showBranch: true; showAheadBehind: true; showStaged: true; showModified: true; showUntracked: true; showConflicts: true; };
        time: { enabled: true; format: "HH:mm:ss"; };
    };
    history: SparshHistory = { maxEntries: 10000; dedupeConsecutive: true; };
    completion: SparshCompletion = { enabled: true; };
};

function startup() -> shell {
    return shell {
        nitch;
    };
};
"#;

        let formatted = format_source(src).expect("format");

        assert!(
            formatted.contains(
                r#"import type {
    SparshAlias,
    SparshEnvironmentVariable,
    SparshPrompt,
    SparshHistory,
    SparshCompletion
} from "sparsh-types.spar";"#
            ),
            "type import should wrap cleanly: {formatted}"
        );
        assert!(
            formatted.contains(
                r#"import {
    greet,
    build,
    showFile,
    rsBinInstall,
    zipOccLang
} from "functions.spar";

struct Config"#
            ),
            "imports should stay grouped with one blank line before the struct: {formatted}"
        );
        assert!(
            formatted.contains(
                r#"aliases: List<SparshAlias> = [
        {
            name: "gs";
            command: ["git", "status"];
        },"#
            ),
            "lists of config objects should be vertical: {formatted}"
        );
        assert!(
            formatted.contains(
                r#"environment: List<SparshEnvironmentVariable> = [
        { name: "EDITOR"; value: "nvim"; },
        {
            name: "PATH";
            prepend: ["$HOME/.local/bin", "$HOME/.cargo/bin"];
            append: ["$HOME/.pub-cache/bin"];
        }
    ];"#
            ),
            "environment PATH edits should format as a readable nested object: {formatted}"
        );
        assert!(
            formatted.contains(
                r#"path: {
            enabled: true;
            parentLength: 64;
            maxLastLength: 96;
            maxWidth: 512;
        };"#
            ),
            "larger nested objects should expand vertically: {formatted}"
        );
        assert!(
            formatted.contains(r#"time: { enabled: true; format: "HH:mm:ss"; };"#)
                && formatted.contains(
                    "history: SparshHistory = { maxEntries: 10000; dedupeConsecutive: true; };"
                ),
            "small simple objects should remain compact: {formatted}"
        );
        assert!(
            formatted.contains(
                r#"function startup() -> shell {
    return shell {
        nitch;
    };
};"#
            ),
            "startup() should remain a normal formatted Spar function: {formatted}"
        );
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn formats_explicit_package_imports() {
        let src = concat!(
            "import pkg \"http\" as http;\n",
            "import pkg { get, post as send } from \"http/client\";\n",
        );
        let once = format_source(src).expect("format");
        assert!(once.contains("import pkg \"http\" as http;"), "got: {once}");
        assert!(
            once.contains("import pkg { get, post as send } from \"http/client\";"),
            "got: {once}"
        );
        let twice = format_source(&once).expect("format again");
        assert_eq!(once, twice, "formatting must be idempotent");
    }

    #[test]
    fn formats_module_and_indexed_loop_statements_canonically() {
        let src = "if(true){notify();}\nfor(index,value) in [1,2]{notify();}\n";
        let formatted = format_source(src).expect("format");
        assert!(
            formatted.contains("if (true) {\n    notify();\n}"),
            "got: {formatted}"
        );
        assert!(
            formatted.contains("for (index, value) in [1, 2] {"),
            "got: {formatted}"
        );
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn formats_schema_file_optional_section() {
        let src = "@SchemaFile\nSchema? [Y]{\n    b: str;\n};\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("Schema? [Y]{"),
            "optional schema marker: {}",
            formatted
        );
    }

    #[test]
    fn formats_schema_field_required_and_optional() {
        let src = "@SchemaFile\nSchema [X]{\n    a: int;\n    b?: str;\n};\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("    a: int;"),
            "required field: {}",
            formatted
        );
        assert!(
            formatted.contains("    b?: str;"),
            "optional field: {}",
            formatted
        );
    }

    #[test]
    fn formats_import_schema() {
        let src = r#"import schema "s.spar";"#;
        let formatted = format_source(src).unwrap();
        assert_eq!(formatted.trim(), r#"import schema "s.spar";"#);
    }

    #[test]
    fn formats_nested_section_schema_field() {
        let src = "@SchemaFile\nSchema [X]{\n    x: section = { host: str; };\n};\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("x: section = {"),
            "nested section field: {}",
            formatted
        );
        assert!(
            formatted.contains("host: str;"),
            "nested field: {}",
            formatted
        );
    }

    #[test]
    fn formats_type_decl_round_trip() {
        let src = "type [Border]{\n    width?: int;\n};\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("type Border {"),
            "must contain type header: {}",
            formatted
        );
        assert!(
            formatted.contains("width?: int;"),
            "must contain the optional field: {}",
            formatted
        );
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn formats_export_type_and_named_field_round_trip() {
        let src = "type [Border]{\n    width?: int;\n};\nexport type [Decoration]{\n    border?: Border;\n};\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("export type Decoration {"),
            "must contain export type header: {}",
            formatted
        );
        assert!(
            formatted.contains("border?: Border;"),
            "must contain the named-type field: {}",
            formatted
        );
    }

    #[test]
    fn formats_type_binding_on_section_round_trip() {
        let src = "type [PostgresType]{\n    image: str;\n};\n[Postgres] -> PostgresType {\n    image: str = \"postgres:16\";\n};\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("struct Postgres: PostgresType {"),
            "must round-trip the type binding: {}",
            formatted
        );
    }

    #[test]
    fn preserves_standalone_comments_between_items() {
        let src = "// first comment\nvar x: int = 1;\n// between\nvar y: int = 2;\n";
        let out = fmt(src);
        assert!(out.contains("// first comment"), "leading comment: {out}");
        assert!(out.contains("// between"), "between comment: {out}");
        let x_pos = out.find("var x").unwrap();
        let c_pos = out.find("// between").unwrap();
        let y_pos = out.find("var y").unwrap();
        assert!(
            x_pos < c_pos && c_pos < y_pos,
            "comment between x and y: {out}"
        );
    }

    #[test]
    fn preserves_comments_inside_sections() {
        let src = "[S]{\n    a: int = 1;\n    // commented\n    b: int = 2;\n};\n";
        let out = fmt(src);
        assert!(out.contains("// commented"), "section comment: {out}");
        let a_pos = out.find("a: int").unwrap();
        let c_pos = out.find("// commented").unwrap();
        let b_pos = out.find("b: int").unwrap();
        assert!(
            a_pos < c_pos && c_pos < b_pos,
            "comment between a and b: {out}"
        );
    }

    #[test]
    fn trailing_comment_on_a_lone_top_level_var_is_not_deleted() {
        let src = "var x: int = 1; // trailing note\n";
        let out = fmt(src);
        assert!(
            out.contains("// trailing note"),
            "trailing comment must survive formatting, not be silently dropped: {out}"
        );
    }

    #[test]
    fn trailing_block_comment_on_a_top_level_var_is_not_deleted() {
        let src = "var x: int /* inline */ = 1;\n";
        let out = fmt(src);
        assert!(
            out.contains("/* inline */"),
            "trailing block comment must survive formatting: {out}"
        );
    }

    #[test]
    fn trailing_comment_before_the_next_top_level_item_is_not_deleted() {
        let src = "var x: int = 1; // trailing\nvar y: int = 2;\n";
        let out = fmt(src);
        assert!(
            out.contains("// trailing"),
            "trailing comment must survive formatting: {out}"
        );
        let x_pos = out.find("var x").unwrap();
        let c_pos = out.find("// trailing").unwrap();
        let y_pos = out.find("var y").unwrap();
        assert!(
            x_pos < c_pos && c_pos < y_pos,
            "comment stays between x and y: {out}"
        );
    }

    #[test]
    fn preserves_a_commented_out_field_inside_a_task_body() {
        let src = "task Build {\n    description: \"real\";\n    // description: \"old\";\n    default: true;\n    run { true; };\n};\n";
        let out = fmt(src);
        assert!(out.contains("// description: \"old\";"), "{out}");
        let real_pos = out.find("description: \"real\"").unwrap();
        let comment_pos = out.find("// description: \"old\";").unwrap();
        let default_pos = out.find("default: true").unwrap();
        let closing_pos = out.rfind("};").unwrap();
        assert!(
            real_pos < comment_pos && comment_pos < default_pos,
            "comment must stay between description and default, inside the task: {out}"
        );
        assert!(
            comment_pos < closing_pos,
            "comment leaked outside the task: {out}"
        );
    }

    #[test]
    fn task_body_comment_does_not_leak_into_the_next_task() {
        let src = "task Build {\n    run { true; };\n    // note about build\n};\ntask Next {\n    run { true; };\n};\n";
        let out = fmt(src);
        let build_close = out.find("task Build").unwrap();
        let next_open = out.find("task Next").unwrap();
        let comment_pos = out.find("// note about build").unwrap();
        assert!(
            build_close < comment_pos && comment_pos < next_open,
            "comment must render before `task Next`, still inside Build: {out}"
        );
        // Must be indented as if inside the task body, not at column 0.
        let comment_line = out
            .lines()
            .find(|line| line.contains("// note about build"))
            .unwrap();
        assert!(
            comment_line.starts_with("    "),
            "comment must be indented inside the task: {comment_line:?}"
        );
    }

    #[test]
    fn task_body_comment_after_last_run_block_stays_inside() {
        let src = "task Build {\n    run { true; };\n    // trailing note\n};\n";
        let out = fmt(src);
        let comment_pos = out.find("// trailing note").unwrap();
        let closing_pos = out.rfind("};").unwrap();
        assert!(
            comment_pos < closing_pos,
            "comment leaked past the closing brace: {out}"
        );
    }

    #[test]
    fn preserves_block_comments_inside_a_task_body() {
        let src = "task Build {\n    /* multi\n       line */\n    run { true; };\n};\n";
        let out = fmt(src);
        assert!(out.contains("/* multi"), "{out}");
        assert!(out.contains("line */"), "{out}");
        let comment_pos = out.find("/* multi").unwrap();
        let run_pos = out.find("run").unwrap();
        let closing_pos = out.rfind("};").unwrap();
        assert!(
            comment_pos < run_pos,
            "block comment must precede run block: {out}"
        );
        assert!(
            comment_pos < closing_pos,
            "block comment leaked outside the task: {out}"
        );
    }

    #[test]
    fn formatting_a_task_with_in_body_comments_twice_is_idempotent() {
        let src = "task Build {\n    description: \"real\";\n    // description: \"old\";\n    default: true;\n    run { true; };\n    // trailing\n};\n";
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice, "formatting must be idempotent: {once}");
    }

    #[test]
    fn format_dot_field_access() {
        let src = "var x: str = person.name;\n";
        assert_eq!(fmt(src).trim(), "var x: str = person.name;");
    }

    #[test]
    fn format_dot_chain_after_index() {
        let src = "var x: str = people[0].name;\n";
        assert_eq!(fmt(src).trim(), "var x: str = people[0].name;");
    }

    #[test]
    fn minimal_task_round_trips_and_is_idempotent() {
        let src = "task Build {\n    run {\n        cargo build;\n    };\n};\n";
        let formatted = fmt(src);
        assert_eq!(formatted, src);
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
    }

    #[test]
    fn task_with_all_fields_round_trips_and_is_idempotent() {
        let src = concat!(
            "task Deploy(environment: str) {\n",
            "    description: \"Deploy the app\";\n",
            "    default: true;\n",
            "    quiet: true;\n",
            "    dependsOn: [Build, Test];\n",
            "    cwd: \"./web\";\n",
            "    env: {\n",
            "        RUST_LOG: \"debug\";\n",
            "    };\n",
            "    run bash {\n",
            "        echo \"hi\";\n",
            "        ./deploy.sh ${environment};\n",
            "    };\n",
            "};\n",
        );
        let formatted = fmt(src);
        assert_eq!(formatted, src);
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
    }

    #[test]
    fn task_v2_metadata_and_parameters_format_in_stable_order() {
        let src = concat!(
            "task Deploy(environment: str = \"staging\", *extra: str) {\n",
            "    description: \"Deploy the app\";\n",
            "    default: true;\n",
            "    quiet: true;\n",
            "    private: true;\n",
            "    group: \"release\";\n",
            "    confirm: \"Really deploy?\";\n",
            "    dependsOn: [Build];\n",
            "    cwd: \"./web\";\n",
            "    env: {\n",
            "        RUST_LOG: \"debug\";\n",
            "    };\n",
            "    run bash {\n",
            "        ./deploy.sh ${environment} ${extra};\n",
            "    };\n",
            "};\n",
        );
        let formatted = fmt(src);
        assert_eq!(formatted, src);
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn multiple_run_blocks_format_in_source_order_with_labels() {
        let src = "task T {\n    run {\n        echo default;\n    };\n    run windows {\n        echo win;\n    };\n};\n";
        let formatted = fmt(src);
        assert_eq!(formatted, src);
        let reformatted = fmt(&formatted);
        assert_eq!(formatted, reformatted, "formatting must be idempotent");
    }

    #[test]
    fn task_shell_body_content_stays_stable_through_formatting() {
        let src =
            "task Build {\n    run {\n        cargo build --workspace --release;\n    };\n};\n";
        let formatted = fmt(src);
        assert!(formatted.contains("cargo build --workspace --release;"));
    }

    #[test]
    fn task_hash_escape_round_trips_and_reparses() {
        let src = concat!(
            "task Build {\n",
            "    run bash {\n",
            "        echo #{HOME:-x};\n",
            "    };\n",
            "};\n",
        );
        let formatted = fmt(src);
        assert!(formatted.contains("#{HOME:-x}"), "got: {formatted}");
        assert!(!formatted.contains("${HOME:-x}"), "got: {formatted}");

        let tokens = Lexer::new(&formatted).tokenize().unwrap();
        Parser::new(tokens)
            .parse()
            .expect("formatted hash escape must still parse");
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn shebang_task_script_round_trips_without_an_added_semicolon() {
        let src = concat!(
            "task Script {\n",
            "    run bash {\n",
            "        #!/usr/bin/env bash\n",
            "        echo one\n",
            "        if true; then echo two; fi\n",
            "    };\n",
            "};\n",
        );
        let formatted = fmt(src);
        assert_eq!(formatted, src);
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn formats_task_without_brackets_and_canonical_run_header() {
        let src = "task Build { run bash windows { dir; }; run linux { echo hi; }; };\n";
        let out = fmt(src);
        assert!(out.contains("task Build {"), "{out}");
        assert!(out.contains("run bash windows {"), "{out}");
        assert!(out.contains("run linux {"), "{out}");
        assert_eq!(fmt(&out), out, "formatting must be idempotent");
    }

    #[test]
    fn native_run_body_round_trips() {
        let src = "task T {\n    run {\n        echo hi;\n    };\n};\n";
        assert_eq!(fmt(src), src);
        assert_eq!(fmt(&fmt(src)), fmt(src));
    }

    #[test]
    fn load_env_pragma_formats_first() {
        let src = "@LoadEnv\ntask Build { run { echo build; }; };\n";
        assert_eq!(
            fmt(src),
            "@LoadEnv\n\ntask Build {\n    run {\n        echo build;\n    };\n};\n"
        );
    }

    #[test]
    fn load_env_pragma_formats_a_custom_path() {
        let src = "@LoadEnv(\".env.production\")\ntask Build { run { echo build; }; };\n";
        assert_eq!(
            fmt(src),
            "@LoadEnv(\".env.production\")\n\ntask Build {\n    run {\n        echo build;\n    };\n};\n"
        );
    }

    /// Every comment in the source must survive formatting, in the same
    /// position, and formatting again must change nothing.
    fn assert_comments_stay_put(source: &str) {
        let formatted = fmt(source);
        assert_eq!(formatted, source, "comments moved or layout changed");
        assert_eq!(fmt(&formatted), formatted, "not idempotent");
    }

    #[test]
    fn comments_stay_inside_nested_section_fields() {
        assert_comments_stay_put(
            "struct Container {\n    // above\n    padding: int = 5; // trailing\n    decoration: section = {\n        // inside\n        color: str = \"red\"; // nested\n        // tail nested\n    };\n    // tail\n};\n",
        );
    }

    #[test]
    fn comments_stay_inside_function_bodies_and_blocks() {
        assert_comments_stay_put(
            "function f(x: int) -> int {\n    // head\n    var y: int = x; // t1\n    if y > 1 {\n        // then\n        y = 2;\n        // then tail\n    } else {\n        y = 3; // t2\n        // else tail\n    }\n    for i in [1, 2] {\n        // loop\n        y = i;\n    }\n    try {\n        y = 4;\n    } catch e {\n        // catch\n        y = 5;\n    }\n    return y; // t3\n};\n",
        );
    }

    #[test]
    fn trailing_comment_after_top_level_items_stays_on_its_line() {
        assert_comments_stay_put("var a: int = 1; // one\n\nvar b: int = 2; // two\n");
    }

    #[test]
    fn comments_stay_inside_native_task_run_bodies() {
        assert_comments_stay_put(
            "task Deploy {\n    run {\n        // head\n        var n: str = \"a\"; // t1\n        echo \"${n}\";\n        // tail\n    };\n};\n",
        );
        assert_comments_stay_put(
            "task Cmds {\n    run {\n        // first\n        echo a; // ta\n        // second\n        echo b && echo c; // tb\n        // last\n    };\n};\n",
        );
    }

    #[test]
    fn comments_stay_inside_shell_values() {
        assert_comments_stay_put(
            "function g() -> shell {\n    return shell {\n        // head\n        var n: str = \"a\"; // t1\n        echo hi;\n        // tail\n    };\n};\n",
        );
    }

    #[test]
    fn comments_stay_inside_object_literals() {
        assert_comments_stay_put(
            "var cfg: Server = {\n    // above host\n    host: \"h\"; // trailing\n    // between\n    port: 80;\n    // tail\n};\n",
        );
        assert_comments_stay_put(
            "var cfg: Server = {\n    host: \"h\";\n    limits: {\n        // inside nested\n        max: 1; // tm\n        // nested tail\n    };\n};\n",
        );
    }

    #[test]
    fn comment_inside_a_short_object_keeps_it_multiline() {
        // Would fit on one line, but a comment cannot live on that line.
        assert_comments_stay_put("var cfg: Server = {\n    // why\n    host: \"h\";\n};\n");
    }

    #[test]
    fn comments_stay_inside_enum_declarations() {
        assert_comments_stay_put(
            "enum Mode {\n    // first\n    Fast, // quick\n    // second\n    Slow\n    // tail\n};\n",
        );
    }

    #[test]
    fn comments_stay_inside_type_declarations() {
        assert_comments_stay_put(
            "type Config {\n    // name\n    name: str; // the name\n    // port\n    port: int = 80;\n    // tail\n};\n",
        );
    }

    #[test]
    fn else_if_chains_round_trip() {
        let source = "function f(a: int) -> int {\n    if a == 1 {\n        return 10;\n    } else if a == 2 {\n        // two\n        return 20;\n    } else {\n        return 30;\n    }\n};\n";
        assert_eq!(fmt(source), source);
    }

    #[test]
    fn mixed_shell_pipeline_formatting_preserves_explicit_bridges() {
        let source = concat!(
            "function main() -> shell {\n",
            "    return shell {\n",
            "        printf '%s\\n' '{\"name\":\"Obi\"}' | from jsonl |> take(1) |> to jsonl | cat;\n",
            "    };\n",
            "};\n",
        );
        let formatted = fmt(source);
        assert!(
            formatted.contains("| from jsonl |> take(1) |> to jsonl | cat"),
            "{formatted}"
        );
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn mixed_shell_decoder_options_and_namespace_round_trip() {
        let source = concat!(
            "function main() -> shell {\n",
            "    var useRaw: bool = true;\n",
            "    return shell {\n",
            "        printf x | from scoc::ping-s(raw: useRaw, ignoreErrors: choose(\"a,b\"));\n",
            "    };\n",
            "};\n",
        );
        let formatted = fmt(source);
        assert!(
            formatted.contains("| from scoc::ping-s(raw: useRaw, ignoreErrors: choose(\"a,b\"))"),
            "{formatted}"
        );
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn structured_pipe_formats_as_a_readable_multiline_chain() {
        let source = "function main() -> int {\n    var result: int = 5 |> double |> fn(value: int) -> int => value + 1;\n    return result;\n};\n";
        let formatted = fmt(source);
        assert!(formatted.contains("var result: int = 5\n        |> double\n        |> fn(value: int) -> int => value + 1;"), "{formatted}");
        assert_eq!(fmt(&formatted), formatted);
    }

    #[test]
    fn an_if_nested_inside_else_stays_nested() {
        let source = "function f(a: int) -> int {\n    if a == 1 {\n        return 10;\n    } else {\n        if a == 2 {\n            return 20;\n        }\n    }\n    return 0;\n};\n";
        assert_eq!(fmt(source), source);
    }
}
