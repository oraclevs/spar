fn parse_ok(src: &str) -> crate::ast::Program {
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    crate::parser::Parser::new(tokens).parse().unwrap()
}

fn parse_err(src: &str) -> String {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    match crate::parser::Parser::new(tokens).parse() {
        Ok(_) => panic!("expected a parse error"),
        Err(e) => format!("{:?}", e),
    }
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
    let src = "@SchemaFile\nSchema [X]{ a: int; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let prog = crate::parser::Parser::new(tokens).parse().unwrap();
    assert!(prog.is_schema_file, "is_schema_file must be true");
}

#[test]
fn parses_required_schema_section() {
    let src = "@SchemaFile\nSchema [X]{ a: int; }";
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
    let src = "@SchemaFile\nSchema? [Y]{ b: str; }";
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
    let src = "@SchemaFile\nSchema [X]{ a: int; b?: str; }";
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
Schema [X]{
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
        crate::ast::TopLevelItem::Import(d) => assert!(matches!(d.kind, crate::ast::ImportKind::Schema)),
        _ => panic!(),
    }
    match &prog.items[1] {
        crate::ast::TopLevelItem::Import(d) => assert!(!matches!(d.kind, crate::ast::ImportKind::Schema)),
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
        }
    "#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    assert!(crate::parser::Parser::new(tokens).parse().is_ok());
}

#[test]
fn schema_section_without_pragma_is_parse_error() {
    // A Schema declaration outside a @SchemaFile is an error.
    let src = "Schema [X]{ a: int; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let result = crate::parser::Parser::new(tokens).parse();
    assert!(result.is_err(), "schema section in non-schema file must be a parse error");
}

#[test]
fn parse_type_decl_with_named_and_nested_fields() {
    let src = r#"
        type [Border]{
            width?: int;
        }
        export type [Decoration]{
            color?: str;
            border?: Border;
            boxShadow: section = {
                blurRadius: int;
            };
        }
    "#;
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 2);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Type(t) => {
            assert_eq!(t.name, "Border");
            assert!(!t.exported);
            assert_eq!(t.fields.len(), 1);
        }
        other => panic!("expected TopLevelItem::Type, got {:?}", other),
    }
    match &prog.items[1] {
        crate::ast::TopLevelItem::Type(t) => {
            assert_eq!(t.name, "Decoration");
            assert!(t.exported);
            assert_eq!(t.fields.len(), 3);
            assert!(matches!(t.fields[1].shape, crate::ast::TypeFieldShape::Named(ref n) if n == "Border"));
            assert!(matches!(t.fields[2].shape, crate::ast::TypeFieldShape::Section(_)));
        }
        other => panic!("expected TopLevelItem::Type, got {:?}", other),
    }
}

#[test]
fn parse_section_with_type_binding() {
    let src = r#"
        type [PostgresType]{
            image: str;
        }
        [Postgres] -> PostgresType {
            image: str = "postgres:16";
        };
    "#;
    let prog = parse_ok(src);
    match &prog.items[1] {
        crate::ast::TopLevelItem::Section(s) => {
            let binding = s.type_binding.as_ref().expect("expected a type_binding");
            assert_eq!(binding.name, "PostgresType");
        }
        other => panic!("expected TopLevelItem::Section, got {:?}", other),
    }
}

#[test]
fn parse_schema_decl_still_requires_schema_file() {
    // Unchanged behavior: a Schema declaration outside @SchemaFile is
    // still rejected, by the SAME check that already exists — this just
    // confirms the keyword-prefix migration didn't disturb it.
    let src = r#"Schema [X]{ a: int; }"#;
    let err = parse_err(src);
    assert!(err.contains("not a schema file") || err.contains("@SchemaFile"),
        "expected the existing schema-file-required error, got: {err}");
}

#[test]
fn parse_angle_bracket_schema_no_longer_parses() {
    // Angle brackets are fully deprecated — the old <Schema> suffix form
    // must no longer parse, even inside a @SchemaFile.
    let src = "@SchemaFile\n[X]<Schema>{ a: int; }\n";
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
fn parse_import_type_selective() {
    let src = r#"import type { PostgresType, Border as B } from "shared.spar";"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Import(d) => match &d.kind {
            crate::ast::ImportKind::TypeSelective(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].name, "PostgresType");
            }
            other => panic!("expected TypeSelective, got {:?}", other),
        },
        other => panic!("expected TopLevelItem::Import, got {:?}", other),
    }
}

#[test]
fn parse_import_as_part_of() {
    let src = r#"import asPartOf "common.spar";"#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Import(d) => {
            assert!(matches!(d.kind, crate::ast::ImportKind::AsPartOf));
            assert_eq!(d.path, "common.spar");
        }
        other => panic!("expected TopLevelItem::Import, got {:?}", other),
    }
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
    let src = concat!(
        "@SchemaFile\n",
        "SchemaFrom [Postgres, PostgresType];\n",
        "SchemaFrom? [Cache, CacheType];\n",
    );
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 2);
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaFrom(sf) => {
            assert_eq!(sf.name, "Postgres");
            assert_eq!(sf.source_type, "PostgresType");
            assert!(!sf.marker.optional);
        }
        other => panic!("expected SchemaFrom, got {:?}", other),
    }
    match &prog.items[1] {
        crate::ast::TopLevelItem::SchemaFrom(sf) => {
            assert!(sf.marker.optional);
        }
        other => panic!("expected SchemaFrom, got {:?}", other),
    }
}

#[test]
fn parse_schema_from_rejected_outside_schema_file() {
    let src = r#"SchemaFrom [Postgres, PostgresType];"#;
    let err = parse_err(src);
    assert!(err.contains("schema file"), "got: {err}");
}

#[test]
fn parse_schema_file_still_rejects_non_type_imports() {
    // Task 5 flips `import type` to legal inside @SchemaFile; every OTHER
    // import form must stay rejected there — asserted now so a regression
    // in Task 5 is caught by an already-passing Task 1 test.
    let src = concat!(
        "@SchemaFile\n",
        "import \"x.spar\" as x;\n",
    );
    let _ = parse_err(src);
}

#[test]
fn parse_schema_file_allows_import_type() {
    let src = concat!(
        "@SchemaFile\n",
        "import type { PostgresType } from \"types.spar\";\n",
        "Schema [Postgres]{ image: str; }\n",
    );
    let prog = parse_ok(src);
    assert_eq!(prog.items.len(), 2);
}

#[test]
fn parse_schema_file_still_rejects_selective_import() {
    let src = concat!(
        "@SchemaFile\n",
        "import { PostgresType } from \"types.spar\";\n",
    );
    let _ = parse_err(src);
}

#[test]
fn parse_schema_file_still_rejects_as_part_of() {
    let src = concat!(
        "@SchemaFile\n",
        "import asPartOf \"types.spar\";\n",
    );
    let _ = parse_err(src);
}

#[test]
fn parse_spread_inside_nested_field_body() {
    let src = r#"
        [Postgres] -> PostgresType {
            image: "postgres:16";
            environment: { ...ProductionEnvironment; };
        };
    "#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Section(sd) => {
            let env_field = sd.items.iter().find_map(|it| {
                if let crate::ast::SectionItem::Field(f) = it {
                    if f.name == "environment" { return Some(f); }
                }
                None
            }).expect("expected an environment field");
            match &env_field.value {
                Some(crate::ast::FieldValue::Nested(items)) => {
                    assert_eq!(items.len(), 1);
                    assert!(matches!(items[0], crate::ast::SectionItem::Spread(_)));
                }
                other => panic!("expected FieldValue::Nested, got {:?}", other),
            }
        }
        other => panic!("expected TopLevelItem::Section, got {:?}", other),
    }
}

#[test]
fn parse_spread_mixed_with_fields_inside_nested_body() {
    let src = r#"
        [Postgres] -> PostgresType {
            image: "postgres:16";
            environment: {
                ...ProductionEnvironment;
                port: "3000";
            };
        };
    "#;
    let prog = parse_ok(src);
    match &prog.items[0] {
        crate::ast::TopLevelItem::Section(sd) => {
            let env_field = sd.items.iter().find_map(|it| {
                if let crate::ast::SectionItem::Field(f) = it {
                    if f.name == "environment" { return Some(f); }
                }
                None
            }).expect("expected an environment field");
            match &env_field.value {
                Some(crate::ast::FieldValue::Nested(items)) => assert_eq!(items.len(), 2),
                other => panic!("expected FieldValue::Nested, got {:?}", other),
            }
        }
        other => panic!("expected TopLevelItem::Section, got {:?}", other),
    }
}

// ── SparType::Named ──────────────────────────────────────────────────────────

#[test]
fn parse_var_decl_with_named_type() {
    let src = "var x: Leaf = someExpr;";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[0] else { panic!("expected var") };
    assert_eq!(v.ty, crate::ast::SparType::Named("Leaf".to_string()));
}

#[test]
fn parse_var_decl_with_list_of_named_type() {
    let src = "var xs: [Leaf] = someExpr;";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Var(v) = &program.items[0] else { panic!("expected var") };
    assert_eq!(v.ty, crate::ast::SparType::List(Box::new(crate::ast::SparType::Named("Leaf".to_string()))));
}

#[test]
fn parse_function_param_and_return_with_named_type() {
    let src = "function f(l: Leaf) -> Leaf { return l; }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Function(f) = &program.items[0] else { panic!("expected function") };
    assert_eq!(f.params[0].ty, crate::ast::SparType::Named("Leaf".to_string()));
    assert_eq!(f.ret, crate::ast::SparType::Named("Leaf".to_string()));
}

#[test]
fn parse_section_field_with_explicit_named_type_and_eq_disambiguates_as_type() {
    let src = "[Tree]{ root: Leaf = someExpr; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Section(s) = &program.items[0] else { panic!("expected section") };
    let crate::ast::SectionItem::Field(f) = &s.items[0] else { panic!("expected field") };
    assert_eq!(f.ty, Some(crate::ast::SparType::Named("Leaf".to_string())));
}

#[test]
fn parse_section_field_bare_ident_no_eq_is_still_untyped_value_regression() {
    let src = "[Man] -> Human { name: someVar; };";
    let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
    let program = crate::parser::Parser::new(tokens).parse().unwrap();
    let crate::ast::TopLevelItem::Section(s) = &program.items[0] else { panic!("expected section") };
    let crate::ast::SectionItem::Field(f) = &s.items[0] else { panic!("expected field") };
    assert_eq!(f.ty, None);
    assert!(matches!(&f.value, Some(crate::ast::FieldValue::Expr(crate::ast::Expr::NamespaceRef(_)))));
}
