fn parse_ok(src: &str) -> crate::ast::Program {
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    crate::parser::Parser::new(tokens).parse().unwrap()
}

#[test]
fn parse_function_decl_str_return() {
    let src = r#"
        function greet(name: str) -> str {
            return name;
        }
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
fn parse_function_decl_section_return() {
    let src = r#"
        function makeServer(host: str, port: int) -> section {
            return { host: str = host; port: int = port; };
        }
    "#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Function(f) => {
            assert_eq!(f.params.len(), 2);
            assert!(matches!(f.ret, crate::ast::SparType::Section));
            // The return statement is now a FuncStmt::Return in body.stmts
            let ret_stmt = f.body.stmts.iter().find_map(|s| {
                if let crate::ast::FuncStmt::Return(rv, _) = s { Some(rv) } else { None }
            }).expect("should have a Return stmt");
            match ret_stmt {
                crate::ast::ReturnValue::SectionBlock(fields) => {
                    assert_eq!(fields.len(), 2);
                }
                _ => panic!("expected SectionBlock"),
            }
        }
        _ => panic!("expected Function"),
    }
}

#[test]
fn parse_named_call() {
    let src = r#"var x: str = greet(name: "world");"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => {
            match v.value.as_ref().unwrap() {
                crate::ast::Expr::Call { args, .. } => {
                    assert_eq!(args.len(), 1);
                    assert_eq!(args[0].param_name, "name");
                }
                _ => panic!("expected Call"),
            }
        }
        _ => panic!("expected Var"),
    }
}

#[test]
fn parse_comparison_expr() {
    let src = r#"var x: bool = a == b;"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => {
            match v.value.as_ref().unwrap() {
                crate::ast::Expr::BinaryOp(b) => {
                    assert_eq!(b.op, crate::ast::BinOp::Eq);
                }
                _ => panic!("expected BinaryOp"),
            }
        }
        _ => panic!(),
    }
}

#[test]
fn parse_unary_not() {
    let src = r#"var x: bool = !flag;"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => {
            match v.value.as_ref().unwrap() {
                crate::ast::Expr::Unary { op, .. } => {
                    assert_eq!(*op, crate::ast::UnOp::Not);
                }
                _ => panic!("expected Unary"),
            }
        }
        _ => panic!(),
    }
}

#[test]
fn parse_comprehension() {
    let src = r#"var y: [str] = for x in items { x };"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Var(v) => {
            match v.value.as_ref().unwrap() {
                crate::ast::Expr::Comprehension { var_name, .. } => {
                    assert_eq!(var_name, "x");
                }
                _ => panic!("expected Comprehension"),
            }
        }
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
        }
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
        }
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
fn bool_type_in_return_section_parses() {
    let src = r#"
        function builderFunc(major: int) -> section {
            if major <= 0 {
                return { error: bool = true; message: str = "bad"; };
            }
            return { error: bool = false; };
        }
    "#;
    assert!(parse_ok_result(src).is_ok(), "bool as type in return block must parse");
}

#[test]
fn bool_builtin_call_in_expression_position_still_works() {
    let src = r#"function f(s: str) -> bool { return bool(s); }"#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn int_float_str_bool_as_types_in_all_positions() {
    let src = r#"
        var a: int = 1;
        var b: float = 1.0;
        var c: str = "x";
        var d: bool = true;
        function f(x: float, y: bool) -> str { return str(x); }
        [S]{ n: int = 0; flag: bool = false; };
    "#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn private_function_parses() {
    let src = r#"private function helper(x: int) -> int { return x; }"#;
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
        }
    "#;
    let prog = parse_ok_result(src).expect("for-loop must parse");
    let crate::ast::TopLevelItem::Function(f) = &prog.items[0] else { panic!() };
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
        }
    "#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn comprehension_expression_still_parses() {
    let src = r#"var tagged: [str] = for n in nums { n };"#;
    assert!(parse_ok_result(src).is_ok());
}

#[test]
fn parses_schema_file_pragma() {
    let src = "@SchemaFile\n[X]<Schema>{ a: int; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    assert!(prog.is_schema_file, "is_schema_file must be true");
}

#[test]
fn parses_required_schema_section() {
    let src = "@SchemaFile\n[X]<Schema>{ a: int; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaSection(s) => {
            assert_eq!(s.name, "X");
            assert!(!s.marker.optional);
            assert_eq!(s.fields.len(), 1);
            assert_eq!(s.fields[0].name, "a");
        }
        other => panic!("expected SchemaSection, got {:?}", std::mem::discriminant(other)),
    }
}

#[test]
fn parses_optional_schema_section() {
    let src = "@SchemaFile\n[Y]<Schema?>{ b: str; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaSection(s) => {
            assert!(s.marker.optional);
        }
        _ => panic!("expected SchemaSection"),
    }
}

#[test]
fn parses_optional_schema_field() {
    let src = "@SchemaFile\n[X]<Schema>{ a: int; b?: str; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaSection(s) => {
            assert_eq!(s.fields[0].optional, false);
            assert_eq!(s.fields[1].optional, true);
        }
        _ => panic!("expected SchemaSection"),
    }
}

#[test]
fn parses_nested_section_schema_field() {
    let src = r#"@SchemaFile
[X]<Schema>{
    x: section = { host: str; port?: int; };
}"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaSection(s) => {
            assert_eq!(s.fields.len(), 1);
            assert_eq!(s.fields[0].name, "x");
            match &s.fields[0].shape {
                crate::ast::SchemaFieldShape::Section(nested) => {
                    assert_eq!(nested.len(), 2);
                    assert_eq!(nested[0].name, "host");
                    assert_eq!(nested[1].name, "port");
                    assert!(nested[1].optional);
                }
                _ => panic!("expected Section shape"),
            }
        }
        _ => panic!("expected SchemaSection"),
    }
}

#[test]
fn parses_import_schema() {
    let src = r#"import schema "s.spar";"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(d.is_schema);
            assert_eq!(d.path, "s.spar");
            assert!(d.alias.is_none());
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
        crate::ast::TopLevelItem::Import(d) => assert!(d.is_schema),
        _ => panic!(),
    }
    match &prog.items[1] {
        crate::ast::TopLevelItem::Import(d) => assert!(!d.is_schema),
        _ => panic!(),
    }
}

#[test]
fn non_schema_file_with_lt_gt_comparison_still_parses() {
    // Regression guard: `<` and `>` are now also schema markers.
    // Confirm they still work as comparison operators in expressions.
    let src = r#"
        function f(a: int, b: int) -> bool {
            return a < b;
        }
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    assert!(crate::parser::Parser::new(tokens).parse().is_ok());
}

#[test]
fn schema_section_without_pragma_is_parse_error() {
    // A `<Schema>` section marker outside a @SchemaFile is an error.
    let src = "[X]<Schema>{ a: int; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let result = crate::parser::Parser::new(tokens).parse();
    assert!(result.is_err(), "schema section in non-schema file must be a parse error");
}
