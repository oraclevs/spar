fn parse_ok(src: &str) -> crate::ast::Program {
    let lexer = crate::lexer::Lexer::new(src);
    let shebang = lexer.shebang().map(str::to_owned);
    let tokens = lexer.tokenize().unwrap();
    let mut program = crate::parser::Parser::new(tokens).parse().unwrap();
    program.shebang = shebang;
    program
}


#[test]
fn parses_any_and_preserves_callable_parameter_names() {
    use crate::ast::{CallableParamType, SparType, TopLevelItem};

    let program = parse_ok("var value: Any = 1; var callback: fn(value: int, label: str) -> bool;");
    let TopLevelItem::Var(value) = &program.items[0] else { panic!("expected Any variable"); };
    assert_eq!(value.ty, SparType::Any);

    let TopLevelItem::Var(callback) = &program.items[1] else { panic!("expected callback variable"); };
    assert_eq!(
        callback.ty,
        SparType::Function {
            params: vec![
                CallableParamType { name: "value".into(), ty: SparType::Int },
                CallableParamType { name: "label".into(), ty: SparType::Str },
            ],
            return_type: Box::new(SparType::Bool),
        }
    );
}

#[test]
fn parses_async_function_and_await_expression() {
    use crate::ast::{Expr, ReturnValue, Statement, TopLevelItem};

    let program = parse_ok(
        "async function main() -> int { var pending: Promise<int> = value(); return await pending; };",
    );
    let TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function");
    };
    assert!(function.is_async);
    let Statement::Return(ReturnValue::Expr(Expr::Await { value, .. }), _) =
        &function.body.stmts[1]
    else {
        panic!("expected awaited return");
    };
    assert!(matches!(
        value.as_ref(),
        Expr::NamespaceRef(path) if path.segments == ["pending"]
    ));
}

#[test]
fn parses_async_function_group_member() {
    use crate::ast::TopLevelItem;

    let program = parse_ok("functionGroup Work { async function value() -> int { return 1; } };");
    let TopLevelItem::FunctionGroup(group) = &program.items[0] else {
        panic!("expected function group");
    };
    assert!(group.functions[0].is_async);
}

#[test]
fn rejects_async_without_function() {
    assert!(parse_err("async var value: int = 1;").contains("'fn'"));
}

fn parse_err(src: &str) -> String {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    match crate::parser::Parser::new(tokens).parse() {
        Ok(_) => panic!("expected a parse error"),
        Err(e) => format!("{:?}", e),
    }
}

#[test]
fn parses_generic_function_type_and_explicit_call() {
    use crate::ast::{Expr, SparType, TopLevelItem, TypeFieldShape};

    let program = parse_ok(
        "struct Pair<T, U> { left: T; right: U; }; \
         function identity<T>(value: T) -> T { return value; }; \
         var answer: int = identity<int>(value: 7);",
    );

    let TopLevelItem::Struct(pair) = &program.items[0] else {
        panic!("expected generic type")
    };
    assert_eq!(
        pair.type_parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>(),
        ["T", "U"]
    );
    assert!(matches!(pair.type_decl().fields[0].shape, TypeFieldShape::Primitive(SparType::TypeParameter(ref name)) if name == "T"));

    let TopLevelItem::Function(identity) = &program.items[1] else {
        panic!("expected generic function")
    };
    assert_eq!(identity.type_parameters[0].name, "T");
    assert_eq!(identity.params[0].ty, SparType::TypeParameter("T".into()));
    assert_eq!(identity.ret, SparType::TypeParameter("T".into()));

    let TopLevelItem::Var(answer) = &program.items[2] else {
        panic!("expected variable")
    };
    let Some(Expr::Call { type_arguments, .. }) = &answer.value else {
        panic!("expected explicit generic call")
    };
    assert_eq!(type_arguments, &[SparType::Int]);
}

#[test]
fn nested_applied_types_and_comparisons_are_unambiguous() {
    use crate::ast::{BinOp, Expr, SparType, TopLevelItem};

    let program = parse_ok(
        "function wrap<T>(value: T) -> Box<Pair<T, str>> { return { value: value; }; }; \
         var less: bool = 1 < 2; var greater: bool = 3 > 2;",
    );
    let TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function")
    };
    assert!(matches!(
        function.ret,
        SparType::Applied { ref name, ref arguments }
            if name == "Box" && matches!(arguments.as_slice(), [SparType::Applied { name, .. }] if name == "Pair")
    ));
    let TopLevelItem::Var(less) = &program.items[1] else {
        panic!()
    };
    assert!(matches!(less.value, Some(Expr::BinaryOp(ref op)) if op.op == BinOp::Lt));
    let TopLevelItem::Var(greater) = &program.items[2] else {
        panic!()
    };
    assert!(matches!(greater.value, Some(Expr::BinaryOp(ref op)) if op.op == BinOp::Gt));
}

#[test]
fn empty_generic_parameter_list_is_rejected() {
    assert!(
        parse_err("function identity<>(value: int) -> int { return value; };")
            .contains("generic parameter list cannot be empty")
    );
}

#[test]
fn parse_function_decl_str_return() {
    let src = r#"
        function greet(name: str) -> str {
            return name;
        };
    "#;
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Function(f) => {
            assert_eq!(f.name, "greet");
            assert_eq!(f.params.len(), 1);
            assert_eq!(f.params[0].name, "name");
        }
        _ => panic!("expected Function"),
    }
}

#[test]
fn parse_call_expression_statements_in_function_blocks() {
    let program = parse_ok(
        r#"
        function sink(value: int) -> int { return value; };
        function main() -> int {
            sink(value: 1);
            if true { sink(value: 2); }
            for item in [3] { sink(value: item); }
            return 0;
        };
        "#,
    );
    let crate::ast::TopLevelItem::Function(main) = &program.items[1] else {
        panic!("expected main function");
    };
    assert!(matches!(
        main.body.stmts.first(),
        Some(crate::ast::Statement::Expression(_, _))
    ));
}

#[test]
fn reject_non_call_expression_statement() {
    let error = parse_err("function main() -> int { 42; return 0; };");
    assert!(
        error.contains("only function calls may be used as expression statements"),
        "{error}"
    );
}

#[test]
fn parse_module_if_for_and_call_statements() {
    let program = parse_ok(
        r#"
        function sink(value: int) -> int { return value; };
        if true { sink(value: 1); }
        for item in [2] { sink(value: item); }
        sink(value: 3);
        "#,
    );
    assert!(matches!(
        &program.items[1],
        crate::ast::TopLevelItem::Statement(crate::ast::Statement::If(_))
    ));
    assert!(matches!(
        &program.items[2],
        crate::ast::TopLevelItem::Statement(crate::ast::Statement::For(_))
    ));
    assert!(matches!(
        &program.items[3],
        crate::ast::TopLevelItem::Statement(crate::ast::Statement::Expression(_, _))
    ));
}

#[test]
fn parse_indexed_for_binding() {
    let program = parse_ok(
        "function first(xs: [str]) -> int { for (index, value) in xs { return index; } return 0; };",
    );
    let crate::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function");
    };
    assert!(matches!(
        &function.body.stmts[0],
        crate::ast::Statement::For(crate::ast::ForStmt {
            binding: crate::ast::ForBinding::Indexed { index_name, value_name, .. },
            ..
        }) if index_name == "index" && value_name == "value"
    ));
}

#[test]
fn parse_break_and_continue_statements() {
    let program = parse_ok("for item in [1] { if true { continue; } break; }");
    let crate::ast::TopLevelItem::Statement(crate::ast::Statement::For(loop_stmt)) =
        &program.items[0]
    else {
        panic!("expected module for statement");
    };
    assert!(matches!(loop_stmt.body[0], crate::ast::Statement::If(_)));
    assert!(matches!(loop_stmt.body[1], crate::ast::Statement::Break(_)));
}

#[test]
fn parse_mutable_declarations_and_assignment() {
    let program = parse_ok("var mut count: int = 0; count = count + 1;");
    let crate::ast::TopLevelItem::Var(declaration) = &program.items[0] else {
        panic!("expected variable declaration");
    };
    assert!(declaration.mutable);
    assert!(matches!(
        program.items[1],
        crate::ast::TopLevelItem::Statement(crate::ast::Statement::Assignment { .. })
    ));
}

#[test]
fn parse_void_return_type_and_bare_return() {
    let program = parse_ok("function doThing() -> void { return; };");
    let crate::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function declaration");
    };
    assert_eq!(function.ret, crate::ast::SparType::Void);
    assert!(matches!(
        function.body.stmts[0],
        crate::ast::Statement::Return(crate::ast::ReturnValue::Void, _)
    ));
}

#[test]
fn parse_void_function_with_implicit_fallthrough() {
    let program = parse_ok("function doThing() -> void { };");
    let crate::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function declaration");
    };
    assert!(function.body.stmts.is_empty());
}

#[test]
fn void_is_rejected_as_a_variable_type() {
    let err = parse_err("var x: void = 0;");
    assert!(err.contains("expected a type"), "got: {err}");
}

#[test]
fn leading_shebang_is_preserved_and_ignored_by_parser() {
    let program = parse_ok("#!/usr/bin/env spar\nfunction main() -> void { return; };");
    assert_eq!(program.shebang.as_deref(), Some("#!/usr/bin/env spar"));
    assert!(matches!(
        program.items[0],
        crate::ast::TopLevelItem::Function(_)
    ));
}

#[test]
fn parse_function_parameter_default() {
    let prog = parse_ok(r#"function greet(name: str = "world") -> str { return name; };"#);
    let crate::ast::TopLevelItem::Function(function) = &prog.items[0] else {
        panic!("expected Function");
    };
    assert!(matches!(
        &function.params[0].default,
        Some(crate::ast::Expr::String(string))
            if matches!(&string.parts[..], [crate::ast::StringPart::Literal(value)] if value == "world")
    ));
}

#[test]
fn rejects_required_function_parameter_after_defaulted_parameter() {
    let error =
        parse_err(r#"function greet(prefix: str = "hello", name: str) -> str { return name; };"#);
    assert!(
        error.contains("a required function parameter cannot follow a parameter with a default"),
        "{error}"
    );
}

#[test]
fn rejects_section_return_types_with_named_type_migration_hint() {
    let src = r#"
        function makeServer(host: str, port: int) -> section {
            return { host: host; port: port; };
        };
    "#;
    let error = parse_err(src);
    assert!(error.contains("section") && (error.contains("named type") || error.contains("Record")), "{error}");
}

#[test]
fn parse_named_call() {
    let src = r#"var x: str = greet(name: "world");"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => match v.value.as_ref().unwrap() {
            crate::ast::Expr::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                assert_eq!(args[0].param_name, "name");
            }
            _ => panic!("expected Call"),
        },
        _ => panic!("expected Var"),
    }
}

#[test]
fn parse_comparison_expr() {
    let src = r#"var x: bool = a == b;"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => match v.value.as_ref().unwrap() {
            crate::ast::Expr::BinaryOp(b) => {
                assert_eq!(b.op, crate::ast::BinOp::Eq);
            }
            _ => panic!("expected BinaryOp"),
        },
        _ => panic!(),
    }
}

#[test]
fn parse_unary_not() {
    let src = r#"var x: bool = !flag;"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => match v.value.as_ref().unwrap() {
            crate::ast::Expr::Unary { op, .. } => {
                assert_eq!(*op, crate::ast::UnOp::Not);
            }
            _ => panic!("expected Unary"),
        },
        _ => panic!(),
    }
}

#[test]
fn parse_shell_block_expression() {
    let program = parse_ok("var x: __shell = __shell { echo hi; };");
    let crate::ast::TopLevelItem::Var(declaration) = &program.items[0] else {
        panic!("expected variable declaration")
    };
    assert!(matches!(
        declaration.value,
        Some(crate::ast::Expr::Shell(_))
    ));
}

#[test]
fn parse_exec_shell_expression() {
    let program =
        parse_ok("function f() -> int { var r: ExecResult = exec __shell { true; }; return 0; };");
    let crate::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function")
    };
    let crate::ast::FuncStmt::LocalVar(declaration) = &function.body.stmts[0] else {
        panic!("expected local variable")
    };
    assert!(matches!(declaration.value, crate::ast::Expr::ExecShell(_)));
}

#[test]
fn parse_local_var_can_infer_exec_shell_result_type() {
    let program = parse_ok(
        "function run() -> int { var result = exec __shell { true; }; return result.exitCode; };",
    );
    let crate::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function");
    };
    let crate::ast::FuncStmt::LocalVar(local) = &function.body.stmts[0] else {
        panic!("expected local variable");
    };
    assert!(local.ty.is_none());
}

#[test]
fn parse_bare_exec_without_shell_is_an_error() {
    let error = parse_err("function f() -> int { var r: int = exec 1; return 0; };");
    assert!(
        error.contains("expected ';'") && error.contains("integer literal"),
        "got: {error}"
    );
}

#[test]
fn parse_main_returning_shell() {
    let program = parse_ok("function main() -> __shell { return __shell { echo hi; }; };");
    let crate::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!("expected function")
    };
    assert_eq!(function.ret, crate::ast::SparType::Shell);
}

#[test]
fn parse_comprehension() {
    let src = r#"var y: [str] = for x in items { x };"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => match v.value.as_ref().unwrap() {
            crate::ast::Expr::Comprehension { var_name, .. } => {
                assert_eq!(var_name, "x");
            }
            _ => panic!("expected Comprehension"),
        },
        _ => panic!(),
    }
}

#[test]
fn parse_bare_if_without_else_is_valid() {
    // else is now optional — a bare if should parse successfully
    let src = r#"
        function f(x: int) -> int {
            if x > 0 { var r: int = x; }
            return x;
        };
    "#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Function(f) => {
            assert_eq!(f.body.stmts.len(), 2); // if + return
            match &f.body.stmts[0] {
                crate::ast::FuncStmt::If(i) => {
                    assert!(i.else_stmts.is_empty());
                }
                _ => panic!("expected If"),
            }
        }
        _ => panic!(),
    }
}

#[test]
fn parse_if_else() {
    let src = r#"
        function f(x: bool) -> str {
            if x { var r: str = "yes"; } else { var r: str = "no"; }
            return r;
        };
    "#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Function(f) => {
            assert_eq!(f.body.stmts.len(), 2); // if + return
            match &f.body.stmts[0] {
                crate::ast::FuncStmt::If(i) => {
                    assert!(!i.then_stmts.is_empty());
                    assert!(!i.else_stmts.is_empty());
                }
                _ => panic!("expected If"),
            }
        }
        _ => panic!(),
    }
}

// ── Phase 11d tests ───────────────────────────────────────────────────────────

fn parse_ok_result(src: &str) -> Result<crate::ast::Program, crate::error::SparError> {
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    crate::parser::Parser::new(tokens).parse()
}

#[test]
fn record_object_return_parses_without_section_syntax() {
    let src = r#"
        function builderFunc(major: int) -> Record {
            if major <= 0 {
                return { error: true; message: "bad"; };
            }
            return { error: false; };
        };
    "#;
    assert!(parse_ok_result(src).is_ok(), "record object return must parse");
}

#[test]
fn bool_builtin_call_in_expression_position_still_works() {
    let src = r#"function f(s: str) -> bool { return bool(value: s); };"#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn int_float_str_bool_as_types_in_all_positions() {
    let src = r#"
        var a: int = 1;
        var b: float = 1.0;
        var c: str = "x";
        var d: bool = true;
        function f(x: float, y: bool) -> str { return str(value: x); };
        struct S { n: int = 0; flag: bool = false; };
    "#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn private_function_parses() {
    let src = r#"private function helper(x: int) -> int { return x; };"#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn private_before_var_is_parse_error() {
    let src = r#"private var x: int = 1;"#;
    assert!(parse_ok_result(src).is_err());
}

#[test]
fn for_loop_statement_parses() {
    let src = r#"
        function f(nums: [int]) -> int {
            for n in nums {
                if n > 0 { return n; }
            }
            return 0;
        };
    "#;
    let prog = parse_ok_result(src).expect("for-loop must parse");
    let crate::ast::TopLevelItem::Function(f) = &prog.items[0] else {
        panic!()
    };
    assert!(matches!(f.body.stmts[0], crate::ast::FuncStmt::For { .. }));
}

#[test]
fn nested_for_loops_parse() {
    let src = r#"
        function f(grid: [[int]]) -> int {
            for row in grid {
                for cell in row {
                    if cell > 100 { return cell; }
                }
            }
            return 0;
        };
    "#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn comprehension_expression_still_parses() {
    let src = r#"var tagged: [str] = for n in nums { n };"#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn schema_declaration_makes_a_schema_file() {
    let prog = parse_ok("schema X { a: int; };");
    assert!(prog.is_schema_file, "is_schema_file must be true");
}

#[test]
fn schema_file_may_only_contain_schema_items_and_type_imports() {
    let err = parse_ok_result("schema X { a: int; };\nvar y: int = 1;").unwrap_err();
    assert!(
        err.to_string().contains("schema files may only contain"),
        "{err}"
    );
}

#[test]
fn old_schema_file_pragma_is_removed_with_a_hint() {
    let err = parse_ok_result("@SchemaFile\nschema X { a: int; };").unwrap_err();
    assert!(
        err.to_string()
            .contains("@SchemaFile was removed; declare `schema Name { ... };`"),
        "{err}"
    );
}

#[test]
fn old_bracket_schema_syntax_is_removed_with_a_hint() {
    let err = parse_ok_result("Schema [X]{ a: int; };").unwrap_err();
    assert!(
        err.to_string()
            .contains("Schema [Name]{...} was replaced by `schema Name { ... };`"),
        "{err}"
    );
}

#[test]
fn a_variable_named_schema_still_works() {
    let prog = parse_ok("var schema: int = 1;");
    assert_eq!(prog.items.len(), 1);
    assert!(!prog.is_schema_file);
}

#[test]
fn parses_required_schema_section() {
    let src = "schema X { a: int; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Schema(s) => {
            assert_eq!(s.name, "X");
            assert_eq!(s.fields.len(), 1);
            assert_eq!(s.fields[0].name, "a");
        }
        other => panic!(
            "expected Schema, got {:?}",
            std::mem::discriminant(other)
        ),
    }
}

#[test]
fn rejects_optional_schema_marker() {
    let error = parse_err("schema? Y { b: str; };");
    assert!(error.contains("Option<T>") && error.contains("removed"), "{error}");
}

#[test]
fn schema_optional_fields_use_option_type() {
    let src = "schema X { a: int; b: Option<str>; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    match &prog.items[0] {
        crate::ast::TopLevelItem::Schema(s) => {
            assert!(matches!(
                &s.fields[1].shape,
                crate::ast::SchemaFieldShape::Type(crate::ast::SparType::Applied { name, arguments })
                    if name == "Option" && arguments == &vec![crate::ast::SparType::Str]
            ));
        }
        _ => panic!("expected Schema"),
    }
}

#[test]
fn rejects_legacy_optional_schema_field_marker() {
    let error = parse_err("schema X { a: int; b?: str; };");
    assert!(error.contains("Option<T>") && error.contains("removed"), "{error}");
}

#[test]
fn rejects_nested_section_schema_field() {
    let error = parse_err(r#"schema X { x: section = { host: str; port?: int; }; };"#);
    assert!(error.contains("section") && error.contains("removed"), "{error}");
}

#[test]
fn parses_import_schema() {
    let src = r#"import schema "s.spar";"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(matches!(d.kind, crate::ast::ImportKind::Schema));
            assert_eq!(d.path, "s.spar");
        }
        _ => panic!("expected Import"),
    }
}

#[test]
fn import_schema_and_aliased_import_coexist() {
    let src = r#"import schema "s.spar"; import "c.spar" as c;"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    assert_eq!(prog.items.len(), 2);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(matches!(d.kind, crate::ast::ImportKind::Schema))
        }
        _ => panic!(),
    }
    match &prog.items[1] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(!matches!(d.kind, crate::ast::ImportKind::Schema))
        }
        _ => panic!(),
    }
}

#[test]
fn non_schema_file_with_lt_gt_comparison_still_parses() {
    // Regression guard: confirm `<`/`>` still work as comparison
    // operators in expressions (no grammar in this language uses them
    // for anything else — Schema/Type conformance markers are keyword-
    // prefix and arrow-based instead).
    let src = r#"
        function f(a: int, b: int) -> bool {
            return a < b;
        };
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    assert!(crate::parser::Parser::new(tokens).parse().is_ok());
}

#[test]
fn parse_type_decl_with_named_fields_and_option() {
    let src = r#"
        struct Border {
            width: Option<int> = none();
        };
        struct Shadow {
            blurRadius: int;
        };
        export struct Decoration {
            color: Option<str> = none();
            border: Option<Border> = none();
            boxShadow: Shadow;
        };
    "#;
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 3);
    let crate::ast::TopLevelItem::Struct(decoration) = &prog.items[2] else {
        panic!("expected Decoration type");
    };
    assert_eq!(decoration.name, "Decoration");
    assert!(decoration.exported);
    assert_eq!(decoration.items.len(), 3);
    assert!(matches!(
        decoration.type_decl().fields[2].shape,
        crate::ast::TypeFieldShape::Primitive(crate::ast::SparType::Named(ref n)) if n == "Shadow"
    ));
}

#[test]
fn rejects_inline_section_type_fields() {
    let error = parse_err(r#"struct Decoration { boxShadow: section = { blurRadius: int; }; };"#);
    assert!(error.contains("section") && error.contains("named"), "{error}");
}

#[test]
fn rejects_removed_struct_type_binding() {
    let error = parse_err("struct Postgres: Shape { image = \"postgres:16\"; };");
    assert!(error.contains("struct type bindings were removed"), "{error}");
}

#[test]
fn parse_angle_bracket_schema_no_longer_parses() {
    // Angle brackets are fully deprecated — the old <Schema> suffix form
    // must no longer parse, even inside a @SchemaFile.
    let src = "[X]<Schema>{ a: int; }\n";
    let _ = parse_err(src);
}

// ── Phase 3: imports ────────────────────────────────────────────────────

#[test]
fn parse_selective_import() {
    let src = r#"import { A, B as C } from "shared.spar";"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Import(d) => match &d.kind {
            crate::ast::ImportKind::Selective(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].name, "A");
                assert_eq!(items[0].alias, None);
                assert_eq!(items[1].name, "B");
                assert_eq!(items[1].alias.as_deref(), Some("C"));
            }
            other => panic!("expected Selective, got {:?}", other),
        },
        other => panic!("expected TopLevelItem::Import, got {:?}", other),
    }
}

#[test]
fn rejects_removed_import_type_selective() {
    let error = parse_err(r#"import type { PostgresType, Border as B } from "shared.spar";"#);
    assert!(error.contains("import type"), "{error}");
}

#[test]
fn parse_import_as_part_of_reports_removed_syntax() {
    let err = parse_err(r#"import asPartOf "common.spar";"#);
    assert!(
        err.contains("asPartOf") && err.contains("removed"),
        "got: {err}"
    );
    assert!(err.contains("import {"), "got: {err}");
}

#[test]
fn parse_aliased_and_schema_imports_unchanged() {
    let src = r#"
        import "db.spar" as db;
        import "nodb.spar";
        import schema "s.spar";
    "#;
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 3);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(matches!(&d.kind, crate::ast::ImportKind::Aliased(Some(a)) if a == "db"));
        }
        other => panic!("expected Import, got {:?}", other),
    }
    match &prog.items[1] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(matches!(&d.kind, crate::ast::ImportKind::Aliased(None)));
        }
        other => panic!("expected Import, got {:?}", other),
    }
    match &prog.items[2] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(matches!(d.kind, crate::ast::ImportKind::Schema));
        }
        other => panic!("expected Import, got {:?}", other),
    }
}

#[test]
fn parse_selective_import_rejects_empty_braces() {
    let src = r#"import { } from "shared.spar";"#;
    let err = parse_err(src);
    assert!(err.contains("at least one"), "got: {err}");
}

#[test]
fn parse_selective_import_requires_from() {
    let src = r#"import { A } "shared.spar";"#;
    let _ = parse_err(src);
}

#[test]
fn parse_schema_from_decl() {
    // Task 1 covers grammar only — a schema file containing `import
    // type {...}` isn't legal until Task 5, so this test sticks to plain
    // `SchemaFrom` declarations (parsing them doesn't require the
    // referenced type to actually exist; that's a Task 6 semantic check).
    let src = "schema Postgres from PostgresType;\n";
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaFrom(sf) => {
            assert_eq!(sf.name, "Postgres");
            assert_eq!(sf.source_type, "PostgresType");
        }
        other => panic!("expected SchemaFrom, got {:?}", other),
    }
}

#[test]
fn rejects_optional_schema_from_marker() {
    let error = parse_err("schema? Cache from CacheType;\n");
    assert!(error.contains("Option<T>") && error.contains("removed"), "{error}");
}

#[test]
fn parse_schema_file_still_rejects_non_type_imports() {
    // Task 5 flips `import type` to legal inside @SchemaFile; every OTHER
    // import form must stay rejected there — asserted now so a regression
    // in Task 5 is caught by an already-passing Task 1 test.
    let src = concat!("schema Y { a: int; };\n", "import \"x.spar\" as x;\n",);
    let _ = parse_err(src);
}

#[test]
fn parse_schema_file_allows_ordinary_import() {
    let src = concat!(
        "import { PostgresType } from \"types.spar\";\n",
        "schema Postgres { image: str; };\n",
    );
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 2);
}

#[test]
fn parse_schema_file_allows_selective_import() {
    let src = concat!(
        "schema Y { a: int; };\n",
        "import { PostgresType } from \"types.spar\";\n",
    );
    let _ = parse_ok(src);
}

#[test]
fn parse_schema_file_rejects_removed_as_part_of_syntax() {
    let src = concat!(
        "schema Y { a: int; };\n",
        "import asPartOf \"types.spar\";\n",
    );
    let _ = parse_err(src);
}

#[test]
fn parse_spread_inside_dynamic_record_field() {
    let src = r#"
        struct Postgres {
            image: str = "postgres:16";
            environment: Record = { ...ProductionEnvironment; };
        };
    "#;
    let prog = parse_ok(src);
    let crate::ast::TopLevelItem::Struct(sd) = &prog.items[0] else { panic!("expected struct") };
    let env_field = sd.items.iter().find_map(|item| match item {
        crate::ast::ObjectItem::Field(field) if field.name == "environment" => Some(field),
        _ => None,
    }).expect("environment field");
    let Some(crate::ast::FieldValue::Expr(crate::ast::Expr::Object(items, _))) = &env_field.value else {
        panic!("expected dynamic object expression, got {:?}", env_field.value);
    };
    assert_eq!(items.len(), 1);
    assert!(matches!(items[0], crate::ast::ObjectItem::Spread(_)));
}

#[test]
fn parse_spread_mixed_with_fields_inside_dynamic_record() {
    let src = r#"
        struct Postgres {
            image: str = "postgres:16";
            environment: Record = {
                ...ProductionEnvironment;
                port: "3000";
            };
        };
    "#;
    let prog = parse_ok(src);
    let crate::ast::TopLevelItem::Struct(sd) = &prog.items[0] else { panic!("expected struct") };
    let env_field = sd.items.iter().find_map(|item| match item {
        crate::ast::ObjectItem::Field(field) if field.name == "environment" => Some(field),
        _ => None,
    }).expect("environment field");
    let Some(crate::ast::FieldValue::Expr(crate::ast::Expr::Object(items, _))) = &env_field.value else {
        panic!("expected dynamic object expression, got {:?}", env_field.value);
    };
    assert_eq!(items.len(), 2);
}

// ── SparType::Named ──────────────────────────────────────────────────────────

#[test]
fn parse_var_decl_with_named_type() {
    let src = "var x: Leaf = someExpr;";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[0] else {
        panic!("expected var")
    };
    assert_eq!(v.ty, crate::ast::SparType::Named("Leaf".to_string()));
}

#[test]
fn parse_var_decl_with_list_of_named_type() {
    let src = "var xs: [Leaf] = someExpr;";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[0] else {
        panic!("expected var")
    };
    assert_eq!(
        v.ty,
        crate::ast::SparType::List(Box::new(crate::ast::SparType::Named("Leaf".to_string())))
    );
}

#[test]
fn parse_function_param_and_return_with_named_type() {
    let src = "function f(l: Leaf) -> Leaf { return l; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Function(f) = &program.items[0] else {
        panic!("expected function")
    };
    assert_eq!(
        f.params[0].ty,
        crate::ast::SparType::Named("Leaf".to_string())
    );
    assert_eq!(f.ret, crate::ast::SparType::Named("Leaf".to_string()));
}

#[test]
fn parse_struct_field_with_explicit_named_type_and_eq_disambiguates_as_type() {
    let src = "struct Tree { root: Leaf = someExpr; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Struct(s) = &program.items[0] else {
        panic!("expected struct")
    };
    let crate::ast::ObjectItem::Field(f) = &s.items[0] else {
        panic!("expected field")
    };
    assert_eq!(f.ty, Some(crate::ast::SparType::Named("Leaf".to_string())));
}

#[test]
fn rejects_bound_struct_field_bare_ident() {
    let error = parse_err("struct Man: Human { name: someVar; };");
    assert!(error.contains("struct type bindings were removed"), "{error}");
}

#[test]
fn parse_bare_object_literal_as_var_value() {
    let src = "var x: Leaf = { name: \"a\"; size: 1; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[0] else {
        panic!("expected var")
    };
    let Some(crate::ast::Expr::Object(items, _)) = &v.value else {
        panic!("expected object literal, got {:?}", v.value)
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn parse_object_literal_inside_list_literal() {
    let src = "var xs: [Leaf] = [{ name: \"a\"; }, { name: \"b\"; }];";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[0] else {
        panic!("expected var")
    };
    let Some(crate::ast::Expr::List(elems, _)) = &v.value else {
        panic!("expected list")
    };
    assert_eq!(elems.len(), 2);
    assert!(matches!(&elems[0], crate::ast::Expr::Object(_, _)));
    assert!(matches!(&elems[1], crate::ast::Expr::Object(_, _)));
}

#[test]
fn parse_object_literal_with_spread() {
    let src = "var x: Leaf = { ...Other; size: 1; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[0] else {
        panic!("expected var")
    };
    let Some(crate::ast::Expr::Object(items, _)) = &v.value else {
        panic!("expected object literal")
    };
    assert!(matches!(&items[0], crate::ast::ObjectItem::Spread(_)));
    assert!(matches!(&items[1], crate::ast::ObjectItem::Field(_)));
}

// ── enum declarations ─────────────────────────────────────────────────────────

#[test]
fn parse_enum_decl_basic() {
    let src = "enum Devices { Ios, Android, Windows, MacOs };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Enum(e) = &program.items[0] else {
        panic!("expected enum")
    };
    assert_eq!(e.name, "Devices");
    assert!(!e.exported);
    assert_eq!(e.variants, vec!["Ios", "Android", "Windows", "MacOs"]);
}

#[test]
fn parse_exported_enum_decl() {
    let src = "export enum Devices { Ios, Android };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Enum(e) = &program.items[0] else {
        panic!("expected enum")
    };
    assert!(e.exported);
}

#[test]
fn parse_enum_decl_trailing_comma_allowed() {
    let src = "enum Devices { Ios, Android, };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Enum(e) = &program.items[0] else {
        panic!("expected enum")
    };
    assert_eq!(e.variants, vec!["Ios", "Android"]);
}

#[test]
fn parse_enum_variant_ref_is_two_segment_namespace_ref() {
    let src = "enum Devices { Android };\nvar x: Devices = Devices::Android;";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[1] else {
        panic!("expected var")
    };
    let Some(crate::ast::Expr::NamespaceRef(nr)) = &v.value else {
        panic!("expected namespace ref")
    };
    assert_eq!(
        nr.segments,
        vec!["Devices".to_string(), "Android".to_string()]
    );
}

#[test]
fn parse_function_group_decl() {
    let src = r#"
        functionGroup EdgeInsect {
            function only() -> int { return 1; }
        };
    "#;
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        crate::ast::TopLevelItem::FunctionGroup(g) => {
            assert_eq!(g.name, "EdgeInsect");
            assert!(!g.is_private);
            assert_eq!(g.functions.len(), 1);
            assert_eq!(g.functions[0].name, "only");
        }
        other => panic!("expected FunctionGroup, got {:?}", other),
    }
}

#[test]
fn parse_private_function_group_with_private_inner_function() {
    let src = r#"
        private functionGroup EdgeInsect {
            function only() -> int { return 1; }
            private function semantic(hor: float, vet: float) -> int { return 2; }
        };
    "#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::FunctionGroup(g) => {
            assert!(g.is_private);
            assert_eq!(g.functions.len(), 2);
            assert!(!g.functions[0].is_private);
            assert!(g.functions[1].is_private);
        }
        other => panic!("expected FunctionGroup, got {:?}", other),
    }
}

#[test]
fn parse_function_group_rejects_nested_function_group() {
    let src = r#"
        functionGroup Outer {
            functionGroup Inner {
                function f() -> int { return 1; }
            };
        };
    "#;
    parse_err(src);
}

#[test]
fn parse_dot_field_access() {
    let src = "var x: str = person.name;";
    let prog = parse_ok(src);
    let crate::ast::TopLevelItem::Var(v) = &prog.items[0] else {
        panic!("expected var")
    };
    let Some(crate::ast::Expr::FieldAccess { field, .. }) = &v.value else {
        panic!("expected FieldAccess, got {:?}", v.value)
    };
    assert_eq!(field, "name");
}

#[test]
fn parse_dot_after_index() {
    // The bug this redesign fixes as a side effect: chaining a field
    // access after an index used to be a parse error.
    let src = "var x: str = people[0].name;";
    let prog = parse_ok(src);
    let crate::ast::TopLevelItem::Var(v) = &prog.items[0] else {
        panic!("expected var")
    };
    let Some(crate::ast::Expr::FieldAccess { base, field, .. }) = &v.value else {
        panic!("expected FieldAccess, got {:?}", v.value)
    };
    assert_eq!(field, "name");
    assert!(
        matches!(base.as_ref(), crate::ast::Expr::Index { .. }),
        "base should be an Index expr, got {:?}",
        base
    );
}

#[test]
fn parse_dot_chain_multi_hop() {
    let src = "var x: str = a.b.c;";
    let prog = parse_ok(src);
    let crate::ast::TopLevelItem::Var(v) = &prog.items[0] else {
        panic!("expected var")
    };
    // a.b.c => FieldAccess{ base: FieldAccess{ base: a, field: "b" }, field: "c" }
    let Some(crate::ast::Expr::FieldAccess { base, field, .. }) = &v.value else {
        panic!("expected outer FieldAccess, got {:?}", v.value)
    };
    assert_eq!(field, "c");
    assert!(matches!(base.as_ref(), crate::ast::Expr::FieldAccess { field, .. } if field == "b"));
}

#[test]
fn parse_dot_after_index_and_index_after_dot() {
    let src = "var x: int = a.b[0];";
    let prog = parse_ok(src);
    let crate::ast::TopLevelItem::Var(v) = &prog.items[0] else {
        panic!("expected var")
    };
    let Some(crate::ast::Expr::Index { source, .. }) = &v.value else {
        panic!("expected outer Index, got {:?}", v.value)
    };
    assert!(matches!(source.as_ref(), crate::ast::Expr::FieldAccess { field, .. } if field == "b"));
}

#[test]
fn parses_try_catch_with_binding() {
    let program =
        parse_ok("function main() -> int { try { return 1; } catch error { return 7; } };");
    let crate::ast::TopLevelItem::Function(function) = &program.items[0] else {
        panic!()
    };
    assert!(matches!(
        function.body.stmts[0],
        crate::ast::FuncStmt::Try(_)
    ));
}

#[test]
fn parses_try_catch_without_binding() {
    parse_ok("function f() -> void { try { return; } catch { return; } };");
}

#[test]
fn parses_canonical_struct_and_list_types() {
    use crate::ast::{SparType, TopLevelItem};
    let program = parse_ok(r#"struct Pair<T, V> { left: T; right: V; }; var example: Pair<str, int> = Pair<str, int>(left: "hello", right: 42); var values: List<str> = ["a", "b"];"#);
    let TopLevelItem::Struct(pair) = &program.items[0] else { panic!("expected struct"); };
    assert_eq!(pair.type_parameters.len(), 2);
    let TopLevelItem::Var(example) = &program.items[1] else { panic!("expected variable"); };
    assert_eq!(example.ty, SparType::Applied { name: "Pair".into(), arguments: vec![SparType::Str, SparType::Int] });
    let TopLevelItem::Var(values) = &program.items[2] else { panic!("expected variable"); };
    assert_eq!(values.ty, SparType::List(Box::new(SparType::Str)));
}

#[test]
fn parses_private_export_struct_and_ignored_catch() {
    use crate::ast::TopLevelItem;

    let program = parse_ok(
        "private struct Internal { debug: bool = true; }; \
         export struct Public { name: str = \"spar\"; }; \
         function main() -> void { try { return; } catch { return; } };",
    );
    let TopLevelItem::Struct(internal) = &program.items[0] else {
        panic!()
    };
    assert!(internal.private);
    let TopLevelItem::Struct(public) = &program.items[1] else {
        panic!()
    };
    assert!(public.exported);
    let TopLevelItem::Function(main) = &program.items[2] else {
        panic!()
    };
    let crate::ast::FuncStmt::Try(try_stmt) = &main.body.stmts[0] else {
        panic!()
    };
    assert_eq!(try_stmt.catch_name, None);
}

#[test]
fn canonical_struct_and_legacy_list_type_parse_together() {
    use crate::ast::{SparType, TopLevelItem};

    let program = parse_ok("struct Legacy { ports: [int] = [1, 2]; };");
    let TopLevelItem::Struct(struct_decl) = &program.items[0] else { panic!() };
    let crate::ast::ObjectItem::Field(field) = &struct_decl.items[0] else { panic!() };
    assert_eq!(field.ty, Some(SparType::List(Box::new(SparType::Int))));
}

#[test]
fn accepts_generic_struct_declarations() {
    let program = parse_ok("struct BoxValue<T> { value: T; };");
    let crate::ast::TopLevelItem::Struct(decl) = &program.items[0] else { panic!("expected struct"); };
    assert_eq!(decl.type_parameters.len(), 1);
    assert_eq!(decl.type_parameters[0].name, "T");
}

#[test]
fn native_shell_words_are_contextual_names_outside_construct_position() {
    let program = parse_ok(
        r#"
        struct Tool {
            command: str;
            exec: str;
            shell: str;
        };
        var command: str = "run";
        var exec: str = command;
        var shell: str = exec;
        var tool: Tool = { command: command; exec: exec; shell: shell; };
        function echoFields(command: str, exec: str, shell: str) -> str {
            return command;
        };
        var result: str = echoFields(command: tool.command, exec: tool.exec, shell: tool.shell);
        "#,
    );
    assert!(!program.items.is_empty());
}

#[test]
fn exec_and_shell_construct_forms_remain_reserved_in_construct_position() {
    let program = parse_ok(
        r#"
        var two: __shell = __shell { echo two; };
        function run() -> __shell { return exec { echo three; }; };
        "#,
    );
    assert_eq!(program.items.len(), 2);
}

#[test]
fn emit_attribute_attaches_to_struct_and_var() {
    use crate::ast::TopLevelItem;
    let program = parse_ok(
        "#[emit]\nstruct Server { port: int = 1; };\n#[emit]\nvar version: str = \"1\";\nstruct Plain { a: int = 1; };\n",
    );
    let TopLevelItem::Struct(server) = &program.items[0] else {
        panic!("struct")
    };
    assert!(server.is_emit());
    let TopLevelItem::Var(version) = &program.items[1] else {
        panic!("var")
    };
    assert!(version.is_emit());
    let TopLevelItem::Struct(plain) = &program.items[2] else {
        panic!("struct")
    };
    assert!(!plain.is_emit());
}

#[test]
fn emit_attribute_works_with_export_and_private_and_stacking() {
    use crate::ast::TopLevelItem;
    let program = parse_ok(
        "#[emit]\n#[emit]\nexport var a: int = 1;\n#[emit]\nprivate struct B { x: int = 1; };\n",
    );
    let TopLevelItem::Var(a) = &program.items[0] else {
        panic!("var")
    };
    assert_eq!(a.attributes.len(), 2);
    let TopLevelItem::Struct(b) = &program.items[1] else {
        panic!("struct")
    };
    assert!(b.is_emit() && b.private);
}

#[test]
fn unknown_attribute_is_rejected_with_valid_names() {
    let err = parse_ok_result("#[serialize]\nstruct A { x: int = 1; };").unwrap_err();
    let text = err.to_string();
    assert!(text.contains("unknown attribute `#[serialize]`"), "{text}");
    assert!(text.contains("valid attributes: emit"), "{text}");
}

#[test]
fn attribute_on_function_is_rejected() {
    let err = parse_ok_result("#[emit]\nfunction f() -> int { return 1; };").unwrap_err();
    assert!(
        err.to_string()
            .contains("only valid on top-level structs and vars"),
        "{err}"
    );
}

#[test]
fn dangling_attribute_at_end_of_file_is_rejected() {
    let err = parse_ok_result("var a: int = 1;\n#[emit]\n").unwrap_err();
    assert!(
        err.to_string()
            .contains("only valid on top-level structs and vars"),
        "{err}"
    );
}

#[test]
fn attribute_on_field_is_rejected() {
    let err = parse_ok_result("struct A {\n    #[emit]\n    x: int = 1;\n};").unwrap_err();
    assert!(
        err.to_string()
            .contains("only valid on top-level structs and vars"),
        "{err}"
    );
}


#[test]
fn rejects_legacy_nullable_question_mark_with_option_migration_hint() {
    for source in [
        "var name?: str;",
        "struct UserType { sex?: str; };",
        "struct User { sex?: str = \"male\"; };",
    ] {
        let error = parse_err(source);
        assert!(error.contains("Option<T>"), "{source}: {error}");
        assert!(error.contains("none()"), "{source}: {error}");
    }
}

#[test]
fn fallback_operator_remains_independent_from_removed_nullable_syntax() {
    use crate::ast::{BinOp, Expr, TopLevelItem};

    let program = parse_ok("var value: int = maybe ?? 7;");
    let TopLevelItem::Var(value) = &program.items[0] else { panic!("expected var"); };
    let Some(Expr::BinaryOp(binary)) = &value.value else { panic!("expected fallback expression"); };
    assert_eq!(binary.op, BinOp::Fallback);
}

#[test]
fn legacy_sections_and_section_type_annotations_are_rejected() {
    let legacy = parse_err("[Server] { port: int = 8080; };");
    assert!(legacy.contains("struct"), "{legacy}");

    let annotation = parse_err("var config: section = { port: 8080; };");
    assert!(annotation.contains("section") && annotation.contains("Record"), "{annotation}");

    let field_shape = parse_err("struct Root { server: section; };");
    assert!(field_shape.contains("named") || field_shape.contains("type"), "{field_shape}");
}

#[test]
fn dynamic_object_literals_remain_expressions_not_sections() {
    use crate::ast::{Expr, TopLevelItem};

    let program = parse_ok("var config: Record = { key: \"value\"; };");
    let TopLevelItem::Var(config) = &program.items[0] else { panic!("expected variable"); };
    assert!(matches!(config.value, Some(Expr::Object(_, _))));
}

#[test]
fn named_type_constructor_is_the_typed_nesting_surface() {
    use crate::ast::{Expr, TopLevelItem};

    let program = parse_ok(
        "struct Address { city: str = \"\"; }; var address: Address = Address(city: \"Awka\");",
    );
    let TopLevelItem::Var(address) = &program.items[1] else { panic!("expected address variable"); };
    let Some(Expr::Call { name, args, .. }) = &address.value else { panic!("expected constructor call"); };
    assert_eq!(name, "Address");
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].param_name, "city");
}

#[test]
fn fn_is_the_canonical_function_declaration_keyword() {
    let program = parse_ok("fn answer(value: int) -> int { return value; };");
    assert!(matches!(program.items[0], crate::ast::TopLevelItem::Function(_)));
}

#[test]
fn function_remains_a_compatibility_alias() {
    let program = parse_ok("function answer(value: int) -> int { return value; };");
    assert!(matches!(program.items[0], crate::ast::TopLevelItem::Function(_)));
}

#[test]
fn pipe_closures_parse_for_zero_and_multiple_parameters() {
    use crate::ast::{ClosureBody, Expr, TopLevelItem};

    let program = parse_ok(
        "var zero: fn() -> int = || 42;\n\
         var add: fn(left: int, right: int) -> int = |left: int, right: int| left + right;",
    );
    let TopLevelItem::Var(zero) = &program.items[0] else { panic!("expected zero closure") };
    assert!(matches!(zero.value, Some(Expr::Closure { ref params, body: ClosureBody::Expr(_), .. }) if params.is_empty()));
    let TopLevelItem::Var(add) = &program.items[1] else { panic!("expected add closure") };
    assert!(matches!(add.value, Some(Expr::Closure { ref params, body: ClosureBody::Expr(_), .. }) if params.len() == 2));
}

#[test]
fn pipe_closure_block_body_and_return_type_parse() {
    let program = parse_ok(
        "var clamp: fn(value: int) -> int = |value: int| -> int { return value; };",
    );
    assert!(matches!(
        &program.items[0],
        crate::ast::TopLevelItem::Var(crate::ast::VarDecl {
            value: Some(crate::ast::Expr::Closure { return_type: Some(crate::ast::SparType::Int), .. }),
            ..
        })
    ));
}

#[test]
fn positional_ordinary_calls_are_rejected_with_named_argument_hint() {
    let error = parse_err("var value: int = parseInt(\"42\");");
    assert!(error.contains("by name") || error.contains("parameter"), "{error}");
}

#[test]
fn named_method_calls_preserve_argument_names() {
    let program = parse_ok("var value: int = items.insert(index: 0, value: 7);");
    let crate::ast::TopLevelItem::Var(var) = &program.items[0] else { panic!("expected var") };
    let Some(crate::ast::Expr::MethodCall { args, .. }) = &var.value else { panic!("expected method call") };
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].param_name, "index");
    assert_eq!(args[1].param_name, "value");
}

#[test]
fn marker_is_not_an_identifier_or_call_head() {
    for src in [
        "var x: int = ~ foo;",
        "function f() -> int { ~(1); return 0; };",
        "function f() -> int { var a: int = 1; a.~; return 0; };",
        "function f() -> int { return ~ foo; };",
    ] {
        assert!(
            crate::lexer::Lexer::new(src).tokenize().is_err()
                || crate::parser::Parser::new(crate::lexer::Lexer::new(src).tokenize().unwrap())
                    .parse()
                    .is_err(),
            "{src} must not parse"
        );
    }
}

#[test]
fn command_is_an_ordinary_call_name_and_struct_field() {
    parse_ok(
        r#"
        struct Tool { command: str = "x"; };
        function command(name: str) -> str { return name; };
        var out: str = command(name: "x");
        "#,
    );
}

#[test]
fn user_source_cannot_spell_the_shell_type() {
    for src in [
        "function main() -> shell { return 0; };",
        "var x: shell = 1;",
        "var p: int = shell { echo hi; };",
    ] {
        let err = crate::parser::Parser::new(crate::lexer::Lexer::new(src).tokenize().unwrap())
            .parse()
            .unwrap_err();
        let text = format!("{err:?}");
        assert!(text.contains("ShellResult"), "{src}: {text}");
    }
}

#[test]
fn internal_spelling_still_parses() {
    let src = "function main() -> int { var p: __shell = __shell { echo hi; }; return 0; };";
    assert!(crate::parser::Parser::new(crate::lexer::Lexer::new(src).tokenize().unwrap())
        .parse()
        .is_ok());
}

fn parse_src(src: &str) -> Result<crate::ast::Program, String> {
    let tokens = crate::lexer::Lexer::new(src).tokenize().map_err(|e| format!("{e:?}"))?;
    crate::parser::Parser::new(tokens).parse().map_err(|e| format!("{e:?}"))
}

#[test]
fn shell_variable_is_legal_before_a_block() {
    for src in [
        "function f() -> int { var shell: bool = true; if shell { return 1; } return 0; };",
        "function f() -> int { var shell: [int] = [1]; for x in shell { return x; } return 0; };",
        "function f() -> int { var shell: int = 1; while shell > 0 { break; } return 0; };",
        "function f() -> int { var shell: int = 1; while shell { break; } return 0; };",
        "function f() -> [int] { var shell: [int] = [1]; return for x in shell { x }; };",
        "function f() -> int { var shell: bool = false; if shell { return 1; } else if shell { return 2; } return 0; };",
    ] {
        parse_src(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    }
}

#[test]
fn shell_block_at_statement_start_gives_migration_error() {
    for src in [
        "function f() -> int { shell { echo hi; }; return 0; };",
        "function f() -> int { var r: int = shell { echo hi; }; return 0; };",
        "function f() -> int { var r: shell = 1; return 0; };",
    ] {
        let err = parse_src(src).unwrap_err();
        assert!(err.contains("ShellResult"), "{src}: {err}");
    }
}

#[test]
fn exec_shell_gives_migration_error() {
    let err = parse_src("function f() -> int { var r: ExecResult = exec shell { true; }; return 0; };").unwrap_err();
    assert!(err.contains("ShellResult") && err.contains('~'), "{err}");
}

#[test]
fn bare_command_in_a_plain_function_suggests_the_marker() {
    let error = parse_err("function main() -> int {\n    echo hi;\n    return 0;\n};");
    assert!(error.contains("only function calls may be used as expression statements"), "{error}");
    assert!(error.contains("~ echo hi") && error.contains("ShellResult"), "{error}");
}

#[test]
fn bare_command_hint_is_not_attached_to_other_expression_statements() {
    let error = parse_err("function main() -> int { 42; return 0; };");
    assert!(!error.contains("ShellResult"), "{error}");
}

#[test]
fn bare_command_with_flag_uses_actual_command_text_in_hint() {
    let error = parse_err("function main() -> int {\n    ls -la;\n    return 0;\n};");
    assert!(error.contains("`~ ls -la`"), "{error}");
}

#[test]
fn bare_command_hint_never_fabricates_command_text() {
    for src in ["echo \"hi\";", "ls --color=auto;", "grep a *.txt;", "echo 007;"] {
        let error = parse_err(&format!("function main() -> int {{\n    {src}\n    return 0;\n}};"));
        assert!(error.contains("ShellResult"), "{src}: {error}");
        assert!(error.contains("`~ <command>`"), "{src}: {error}");
    }
}

#[test]
fn bare_command_hint_is_tentative_and_exact_when_given() {
    let error = parse_err("function main() -> int {\n    x y;\n    return 0;\n};");
    assert!(error.contains("if you meant to run a shell command"), "{error}");
    let error = parse_err("function main() -> int {\n    echo hi;\n    return 0;\n};");
    assert!(error.contains("`~ echo hi`"), "{error}");
    let error = parse_err("function main() -> int {\n    a / b;\n    return 0;\n};");
    assert!(!error.contains("ShellResult"), "{error}");
}

#[test]
fn the_removed_command_keyword_is_named_and_dropped_from_the_hint() {
    let error = parse_err("function main() -> int {\n    command echo hi;\n    return 0;\n};");
    assert!(error.contains("`command` keyword was removed"), "{error}");
    assert!(error.contains("`~ echo hi`"), "{error}");
    assert!(!error.contains("~ command"), "{error}");
}
