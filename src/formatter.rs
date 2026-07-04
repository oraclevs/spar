use crate::ast::*;
use crate::lexer::{CommentTrivia, Lexer};
use crate::parser::Parser;
use crate::error::SparError;

pub struct FormatConfig {
    pub indent_width: usize,
}

impl Default for FormatConfig {
    fn default() -> Self {
        FormatConfig { indent_width: 4 }
    }
}

pub fn format_source(src: &str) -> Result<String, SparError> {
    let (tokens, comments) = Lexer::new(src).tokenize_with_comments()?;
    let program = Parser::new(tokens).parse()?;
    Ok(format_program_with_comments(&program, &FormatConfig::default(), &comments))
}

pub fn format_program(program: &Program, config: &FormatConfig) -> String {
    format_program_with_comments(program, config, &[])
}

pub fn format_program_with_comments(program: &Program, config: &FormatConfig, comments: &[CommentTrivia]) -> String {
    let mut out = String::new();
    let mut cx = CommentCursor::new(comments);

    if program.is_schema_file {
        out.push_str("@SchemaFile\n");
    }

    for (i, item) in program.items.iter().enumerate() {
        let item_line = item_span_line(item);
        if i > 0 || program.is_schema_file {
            out.push('\n');
        }
        // Emit any standalone comments preceding this item (after the blank-line separator)
        cx.emit_before_line(item_line, 0, config, &mut out);
        format_top_level_item_cx(item, config, &mut cx, &mut out);
    }
    // Emit any trailing comments at end of file
    cx.emit_before_line(u32::MAX, 0, config, &mut out);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

struct CommentCursor<'a> {
    comments: &'a [CommentTrivia],
    next: usize,
}

impl<'a> CommentCursor<'a> {
    fn new(comments: &'a [CommentTrivia]) -> Self {
        Self { comments, next: 0 }
    }

    /// Emit all pending standalone comments whose source line < `before_line`.
    fn emit_before_line(&mut self, before_line: u32, depth: usize, config: &FormatConfig, out: &mut String) {
        while self.next < self.comments.len() {
            let c = &self.comments[self.next];
            if c.line >= before_line { break; }
            if !c.is_trailing {
                let ind = indent(depth, config);
                out.push_str(&ind);
                out.push_str(&c.text);
                out.push('\n');
            }
            self.next += 1;
        }
    }

    /// Consume a trailing comment on the given source line (if any), returning its text.
    fn take_trailing(&mut self, on_line: u32) -> Option<String> {
        if self.next < self.comments.len() {
            let c = &self.comments[self.next];
            if c.is_trailing && c.line == on_line {
                let text = c.text.clone();
                self.next += 1;
                return Some(text);
            }
        }
        None
    }
}

fn item_span_line(item: &TopLevelItem) -> u32 {
    match item {
        TopLevelItem::Import(d)       => d.span.line,
        TopLevelItem::Var(d)          => d.span.line,
        TopLevelItem::Dynamic(d)      => d.span.line,
        TopLevelItem::Section(d)      => d.span.line,
        TopLevelItem::Function(d)     => d.span.line,
        TopLevelItem::SchemaSection(d) => d.span.line,
    }
}

fn format_top_level_item(item: &TopLevelItem, config: &FormatConfig, out: &mut String) {
    match item {
        TopLevelItem::Import(imp) => {
            if imp.is_schema {
                out.push_str("import schema \"");
                out.push_str(&escape_string_content(&imp.path));
                out.push_str("\";\n");
            } else {
                out.push_str("import \"");
                out.push_str(&escape_string_content(&imp.path));
                out.push('"');
                if let Some(alias) = &imp.alias {
                    out.push_str(" as ");
                    out.push_str(alias);
                }
                out.push_str(";\n");
            }
        }

        TopLevelItem::Var(vd) => {
            if vd.exported { out.push_str("export "); }
            out.push_str("var ");
            out.push_str(&vd.name);
            if vd.optional { out.push('?'); }
            out.push_str(": ");
            out.push_str(&format_type(&vd.ty));
            if let Some(val) = &vd.value {
                out.push_str(" = ");
                format_expr(val, 0, out);
            }
            out.push_str(";\n");
        }

        TopLevelItem::Dynamic(dd) => {
            out.push_str("dynamic var ");
            out.push_str(&dd.name);
            if dd.optional { out.push('?'); }
            if let Some(val) = &dd.value {
                out.push_str(" = ");
                format_expr(val, 0, out);
            }
            out.push_str(";\n");
        }

        TopLevelItem::Section(sd) => {
            if sd.exported { out.push_str("export "); }
            if sd.private  { out.push_str("private "); }
            out.push('[');
            out.push_str(&sd.path.join("."));
            out.push_str("]{\n");
            format_section_items(&sd.items, 1, config, out);
            out.push_str("};\n");
        }

        TopLevelItem::Function(fd) => {
            if fd.is_private { out.push_str("private "); }
            out.push_str("function ");
            out.push_str(&fd.name);
            out.push('(');
            for (i, p) in fd.params.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                out.push_str(&p.name);
                out.push_str(": ");
                out.push_str(&format_type(&p.ty));
            }
            out.push_str(") -> ");
            out.push_str(&format_type(&fd.ret));
            out.push_str(" {\n");
            format_func_stmts(&fd.body.stmts, 1, config, out);
            out.push_str("}\n");
        }

        TopLevelItem::SchemaSection(sd) => {
            out.push('[');
            out.push_str(&sd.name);
            out.push_str("]<Schema");
            if sd.marker.optional { out.push('?'); }
            out.push_str(">{\n");
            for field in &sd.fields {
                format_schema_field(field, 1, config, out);
            }
            out.push_str("}\n");
        }
    }
}

fn format_top_level_item_cx(item: &TopLevelItem, config: &FormatConfig, cx: &mut CommentCursor, out: &mut String) {
    match item {
        TopLevelItem::Section(sd) => {
            if sd.exported { out.push_str("export "); }
            if sd.private  { out.push_str("private "); }
            out.push('[');
            out.push_str(&sd.path.join("."));
            out.push_str("]{\n");
            format_section_items_cx(&sd.items, 1, config, cx, out);
            out.push_str("};\n");
        }
        _ => format_top_level_item(item, config, out),
    }
}

fn format_section_items_cx(items: &[SectionItem], depth: usize, config: &FormatConfig, cx: &mut CommentCursor, out: &mut String) {
    for item in items {
        match item {
            SectionItem::Field(fd) => {
                cx.emit_before_line(fd.span.line, depth, config, out);
                format_field_decl(fd, depth, config, out);
                // Append trailing comment for this line if present
                if let Some(trailing) = cx.take_trailing(fd.span.line) {
                    // Insert before the last \n
                    if out.ends_with('\n') {
                        out.pop();
                        out.push(' ');
                        out.push_str(&trailing);
                        out.push('\n');
                    }
                }
            }
            SectionItem::Spread(ss) => {
                cx.emit_before_line(ss.span.line, depth, config, out);
                let ind = indent(depth, config);
                out.push_str(&ind);
                out.push_str("...");
                format_expr(&ss.expr, 0, out);
                out.push_str(";\n");
            }
        }
    }
}

fn format_type(ty: &SparType) -> String {
    match ty {
        SparType::Str          => "str".to_string(),
        SparType::Int          => "int".to_string(),
        SparType::Float        => "float".to_string(),
        SparType::Bool         => "bool".to_string(),
        SparType::Section      => "section".to_string(),
        SparType::List(inner)  => format!("[{}]", format_type(inner)),
    }
}

fn binop_symbol(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add      => "+",
        BinOp::Sub      => "-",
        BinOp::Mul      => "*",
        BinOp::Div      => "/",
        BinOp::Fallback => "??",
        BinOp::Eq       => "==",
        BinOp::NotEq    => "!=",
        BinOp::Lt       => "<",
        BinOp::Gt       => ">",
        BinOp::LtEq     => "<=",
        BinOp::GtEq     => ">=",
        BinOp::And      => "&&",
        BinOp::Or       => "||",
    }
}

// Higher number = tighter binding. Used to decide when parens are needed.
fn binop_prec(op: &BinOp) -> u8 {
    match op {
        BinOp::Fallback => 1,
        BinOp::Or       => 2,
        BinOp::And      => 3,
        BinOp::Eq | BinOp::NotEq
        | BinOp::Lt | BinOp::Gt
        | BinOp::LtEq | BinOp::GtEq => 4,
        BinOp::Add | BinOp::Sub      => 5,
        BinOp::Mul | BinOp::Div      => 6,
    }
}

fn escape_string_content(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"'  => { out.push('\\'); out.push('"'); }
            '\\' => { out.push('\\'); out.push('\\'); }
            '\n' => { out.push('\\'); out.push('n'); }
            '\t' => { out.push('\\'); out.push('t'); }
            _    => out.push(c),
        }
    }
    out
}

fn format_expr(expr: &Expr, parent_prec: u8, out: &mut String) {
    match expr {
        Expr::Literal(lit) => match lit {
            Literal::Int(n)   => out.push_str(&n.to_string()),
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
            Literal::Bool(b)  => out.push_str(if *b { "true" } else { "false" }),
        },

        Expr::String(is) => {
            out.push('"');
            for part in &is.parts {
                match part {
                    StringPart::Literal(s) => out.push_str(&escape_string_content(s)),
                    StringPart::Expr(e)    => {
                        out.push_str("${");
                        format_expr(e, 0, out);
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
                if i > 0 { out.push_str(", "); }
                format_expr(arg, 0, out);
            }
            out.push(')');
        }

        Expr::Call { name, args, .. } => {
            out.push_str(name);
            out.push('(');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                out.push_str(&arg.param_name);
                out.push_str(": ");
                format_expr(&arg.value, 0, out);
            }
            out.push(')');
        }

        Expr::BinaryOp(b) => {
            let prec = binop_prec(&b.op);
            let needs_parens = prec < parent_prec;
            if needs_parens { out.push('('); }
            format_expr(&b.lhs, prec, out);
            out.push(' ');
            out.push_str(binop_symbol(&b.op));
            out.push(' ');
            // Right side: use prec+1 so same-precedence right operand gets parens
            // (avoids ambiguity for non-associative ops like comparisons)
            format_expr(&b.rhs, prec + 1, out);
            if needs_parens { out.push(')'); }
        }

        Expr::Unary { op, operand, .. } => {
            match op {
                UnOp::Not => out.push('!'),
                UnOp::Neg => out.push('-'),
            }
            // Unary binds tighter than all binary ops (prec 7)
            format_expr(operand, 7, out);
        }

        Expr::List(items, _) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 { out.push_str(", "); }
                format_expr(item, 0, out);
            }
            out.push(']');
        }

        Expr::Grouped(inner, _) => {
            out.push('(');
            format_expr(inner, 0, out);
            out.push(')');
        }

        Expr::Comprehension { var_name, source, body, .. } => {
            out.push_str("for ");
            out.push_str(var_name);
            out.push_str(" in ");
            format_expr(source, 0, out);
            out.push_str(" { ");
            format_expr(body, 0, out);
            out.push_str(" }");
        }

        Expr::Index { source, index, .. } => {
            format_expr(source, 8, out); // 8 = tightest: index always binds to immediate source
            out.push('[');
            format_expr(index, 0, out);
            out.push(']');
        }
    }
}

fn indent(depth: usize, config: &FormatConfig) -> String {
    " ".repeat(depth * config.indent_width)
}

fn format_func_stmts(stmts: &[FuncStmt], depth: usize, config: &FormatConfig, out: &mut String) {
    for stmt in stmts {
        format_func_stmt(stmt, depth, config, out);
    }
}

fn format_func_stmt(stmt: &FuncStmt, depth: usize, config: &FormatConfig, out: &mut String) {
    let ind = indent(depth, config);
    match stmt {
        FuncStmt::LocalVar(lv) => {
            out.push_str(&ind);
            out.push_str("var ");
            out.push_str(&lv.name);
            out.push_str(": ");
            out.push_str(&format_type(&lv.ty));
            out.push_str(" = ");
            format_expr(&lv.value, 0, out);
            out.push_str(";\n");
        }

        FuncStmt::Return(rv, _) => {
            out.push_str(&ind);
            out.push_str("return ");
            match rv {
                ReturnValue::Expr(e) => {
                    format_expr(e, 0, out);
                    out.push_str(";\n");
                }
                ReturnValue::SectionBlock(fields) => {
                    out.push_str("{\n");
                    for rf in fields {
                        out.push_str(&indent(depth + 1, config));
                        out.push_str(&rf.name);
                        out.push_str(": ");
                        out.push_str(&format_type(&rf.ty));
                        out.push_str(" = ");
                        format_expr(&rf.value, 0, out);
                        out.push_str(";\n");
                    }
                    out.push_str(&ind);
                    out.push_str("};\n");
                }
            }
        }

        FuncStmt::If(if_stmt) => {
            out.push_str(&ind);
            out.push_str("if ");
            format_expr(&if_stmt.condition, 0, out);
            out.push_str(" {\n");
            format_func_stmts(&if_stmt.then_stmts, depth + 1, config, out);
            if if_stmt.else_stmts.is_empty() {
                out.push_str(&ind);
                out.push_str("}\n");
            } else {
                out.push_str(&ind);
                out.push_str("} else {\n");
                format_func_stmts(&if_stmt.else_stmts, depth + 1, config, out);
                out.push_str(&ind);
                out.push_str("}\n");
            }
        }

        FuncStmt::For { var_name, iterable, body, .. } => {
            out.push_str(&ind);
            out.push_str("for ");
            out.push_str(var_name);
            out.push_str(" in ");
            format_expr(iterable, 0, out);
            out.push_str(" {\n");
            format_func_stmts(body, depth + 1, config, out);
            out.push_str(&ind);
            out.push_str("}\n");
        }
    }
}

fn format_schema_field(field: &SchemaField, depth: usize, config: &FormatConfig, out: &mut String) {
    let indent = " ".repeat(depth * config.indent_width);
    out.push_str(&indent);
    out.push_str(&field.name);
    if field.optional { out.push('?'); }
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

fn format_field_decl(fd: &FieldDecl, depth: usize, config: &FormatConfig, out: &mut String) {
    let ind = indent(depth, config);
    out.push_str(&ind);
    out.push_str(&fd.name);
    if fd.optional { out.push('?'); }
    out.push_str(": ");
    out.push_str(&format_type(&fd.ty));
    match &fd.value {
        None => { out.push_str(";\n"); }
        Some(FieldValue::Expr(e)) => {
            out.push_str(" = ");
            format_expr(e, 0, out);
            out.push_str(";\n");
        }
        Some(FieldValue::Nested(nested_fields)) => {
            out.push_str(" = {\n");
            for nf in nested_fields {
                format_field_decl(nf, depth + 1, config, out);
            }
            out.push_str(&ind);
            out.push_str("};\n");
        }
    }
}

fn format_section_items(items: &[SectionItem], depth: usize, config: &FormatConfig, out: &mut String) {
    for item in items {
        match item {
            SectionItem::Field(fd) => format_field_decl(fd, depth, config, out),
            SectionItem::Spread(ss) => {
                let ind = indent(depth, config);
                out.push_str(&ind);
                out.push_str("...");
                format_expr(&ss.expr, 0, out);
                out.push_str(";\n");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        format_source(src).expect("format_source failed")
    }

    #[test]
    fn formats_simple_var() {
        assert_eq!(fmt("var x: int = 1;").trim(), "var x: int = 1;");
    }

    #[test]
    fn normalizes_extra_spaces_around_colon_and_eq() {
        assert_eq!(fmt("var   x:int=1;").trim(), "var x: int = 1;");
    }

    #[test]
    fn export_var_has_export_prefix() {
        assert_eq!(fmt("export var x: int = 1;").trim(), "export var x: int = 1;");
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
        assert_eq!(fmt(r#"import "a.spar" as a;"#).trim(), r#"import "a.spar" as a;"#);
    }

    #[test]
    fn section_with_one_field() {
        let out = fmt("[Server]{ port: int = 8080; };");
        assert!(out.contains("[Server]{"));
        assert!(out.contains("    port: int = 8080;"));
        assert!(out.contains("};"));
    }

    #[test]
    fn private_section_has_private_prefix() {
        let out = fmt("private [S]{ x: int = 1; };");
        assert!(out.trim_start().starts_with("private [S]{"));
    }

    #[test]
    fn export_section_has_export_prefix() {
        let out = fmt("export [S]{ x: int = 1; };");
        assert!(out.trim_start().starts_with("export [S]{"));
    }

    #[test]
    fn nested_section_path_joined_with_dot() {
        // The parser only allows single-segment section names (dotted paths are rejected at
        // parse time). The formatter uses .join(".") on the Vec<String> path, which is correct
        // for the AST representation. This test verifies the bracket-wrapping with a valid input.
        let out = fmt("[A]{ x: int = 1; };");
        assert!(out.contains("[A]{"));
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
        let src = "function f(x: int) -> int { return x; }";
        let out = fmt(src);
        assert!(out.contains("function f(x: int) -> int {"));
        assert!(out.contains("    return x;"));
        assert!(out.contains("}"));
    }

    #[test]
    fn private_function_has_private_prefix() {
        let src = "private function f(x: int) -> int { return x; }";
        let out = fmt(src);
        assert!(out.trim_start().starts_with("private function f"));
    }

    #[test]
    fn function_if_else_formatted() {
        let src = "function f(x: bool) -> int { if x { return 1; } else { return 0; } }";
        let out = fmt(src);
        assert!(out.contains("    if x {"));
        assert!(out.contains("        return 1;"));
        assert!(out.contains("    } else {"));
        assert!(out.contains("        return 0;"));
        assert!(out.contains("    }"));
    }

    #[test]
    fn function_for_loop_formatted() {
        let src = "function f(xs: [int]) -> int { for x in xs { return x; } return 0; }";
        let out = fmt(src);
        assert!(out.contains("    for x in xs {"));
        assert!(out.contains("        return x;"));
        assert!(out.contains("    }"));
    }

    #[test]
    fn function_section_return_formatted() {
        let src = "function f(x: int) -> section { return { v: int = x; }; }";
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
        assert_eq!(fmt("var xs: [int] = [1, 2, 3];").trim(), "var xs: [int] = [1, 2, 3];");
    }

    #[test]
    fn namespace_ref_formatted() {
        assert_eq!(fmt("var x: int = A::b::c;").trim(), "var x: int = A::b::c;");
    }

    #[test]
    fn fn_call_positional_formatted() {
        assert_eq!(fmt("var x: str = env(\"PORT\");").trim(), "var x: str = env(\"PORT\");");
    }

    #[test]
    fn call_named_args_formatted() {
        assert_eq!(
            fmt("var x: str = greet(name: \"world\");").trim(),
            "var x: str = greet(name: \"world\");"
        );
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
        assert_eq!(fmt("var x: [[int]] = [];").trim(), "var x: [[int]] = [];");
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
}
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
        assert!(!out.contains("\n\n\n"), "more than one consecutive blank line found");
    }

    #[test]
    fn string_with_escapes_roundtrips() {
        // Quoted chars must survive format → parse → format unchanged.
        let src = r#"var x: str = "say \"hi\"";"#;
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "idempotency broken on escape sequences");
        assert!(once.contains(r#"\"hi\""#), "escaped quote must be re-escaped in output");
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
            items: vec![TopLevelItem::Import(ImportDecl {
                path: "dir\\file.spar".to_string(), // stored with literal backslash
                alias: Some("x".to_string()),
                is_schema: false,
                span: crate::error::Span::dummy(),
            })],
        };
        let out = format_program(&program, &FormatConfig::default());
        assert!(out.contains(r#"import "dir\\file.spar""#), "backslash must be re-escaped: {}", out);
    }

    #[test]
    fn section_path_join_with_dot() {
        use crate::ast::*;
        // Hand-craft a SectionDecl with path = ["A", "B"] to verify .join(".")
        // (the parser rejects "[A.B]" in source, but the AST can represent it)
        let program = Program {
            is_schema_file: false,
            items: vec![TopLevelItem::Section(SectionDecl {
                exported: false,
                private: false,
                path: vec!["A".to_string(), "B".to_string()],
                items: vec![],
                span: crate::error::Span::dummy(),
            })],
        };
        let out = format_program(&program, &FormatConfig::default());
        assert!(out.contains("[A.B]{"), "multi-segment path must be joined with '.'");
    }

    #[test]
    fn formats_schema_file_with_pragma() {
        let src = "@SchemaFile\n[X]<Schema>{\n    a: int;\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.starts_with("@SchemaFile\n"), "must start with @SchemaFile pragma: {}", formatted);
        assert!(formatted.contains("[X]<Schema>{"), "must contain schema section header: {}", formatted);
    }

    #[test]
    fn formats_schema_file_optional_section() {
        let src = "@SchemaFile\n[Y]<Schema?>{\n    b: str;\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("[Y]<Schema?>{"), "optional schema marker: {}", formatted);
    }

    #[test]
    fn formats_schema_field_required_and_optional() {
        let src = "@SchemaFile\n[X]<Schema>{\n    a: int;\n    b?: str;\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("    a: int;"), "required field: {}", formatted);
        assert!(formatted.contains("    b?: str;"), "optional field: {}", formatted);
    }

    #[test]
    fn formats_import_schema() {
        let src = r#"import schema "s.spar";"#;
        let formatted = format_source(src).unwrap();
        assert_eq!(formatted.trim(), r#"import schema "s.spar";"#);
    }

    #[test]
    fn formats_nested_section_schema_field() {
        let src = "@SchemaFile\n[X]<Schema>{\n    x: section = { host: str; };\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("x: section = {"), "nested section field: {}", formatted);
        assert!(formatted.contains("host: str;"), "nested field: {}", formatted);
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
        assert!(x_pos < c_pos && c_pos < y_pos, "comment between x and y: {out}");
    }

    #[test]
    fn preserves_comments_inside_sections() {
        let src = "[S]{\n    a: int = 1;\n    // commented\n    b: int = 2;\n};\n";
        let out = fmt(src);
        assert!(out.contains("// commented"), "section comment: {out}");
        let a_pos = out.find("a: int").unwrap();
        let c_pos = out.find("// commented").unwrap();
        let b_pos = out.find("b: int").unwrap();
        assert!(a_pos < c_pos && c_pos < b_pos, "comment between a and b: {out}");
    }
}
