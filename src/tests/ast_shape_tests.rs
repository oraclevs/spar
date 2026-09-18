#[test]
fn ast_new_types_compile() {
    use crate::ast::*;
    use crate::error::Span;
    // Just constructing to verify the shapes exist
    let _op = UnOp::Not;
    let _binop = BinOp::Eq;
    let _rv = ReturnValue::Expr(Expr::Literal(Literal::Bool(true)));
    let _ca = CallArg {
        param_name: "x".into(),
        param_name_span: Span::dummy(),
        value: Expr::Literal(Literal::Int(1)),
        span: Span::dummy(),
    };
    let _fd = FunctionDecl {
        name: "f".into(),
        name_span: Span::dummy(),
        type_parameters: vec![],
        params: vec![],
        ret: SparType::Bool,
        ret_span: Span::dummy(),
        body: FunctionBody {
            stmts: vec![FuncStmt::Return(
                ReturnValue::Expr(Expr::Literal(Literal::Bool(true))),
                Span::dummy(),
            )],
            span: Span::dummy(),
        },
        is_async: false,
        is_private: false,
        trusted_native: false,
        span: Span::dummy(),
    };

    let _await = Expr::Await {
        value: Box::new(Expr::NamespaceRef(NamespaceRef {
            segments: vec!["pending".into()],
            span: Span::dummy(),
        })),
        span: Span::dummy(),
    };
    let _parameter = TypeParameter {
        name: "T".into(),
        span: Span::dummy(),
    };
    let _applied = SparType::Applied {
        name: "Box".into(),
        arguments: vec![SparType::Int],
    };
    let _ = _fd;
}

#[test]
fn function_control_flow_uses_shared_statement_type() {
    use crate::ast::{FuncStmt, Statement};

    fn accepts_shared_statement(_: &Statement) {}

    let statement = FuncStmt::Return(
        crate::ast::ReturnValue::Expr(crate::ast::Expr::Literal(crate::ast::Literal::Int(0))),
        crate::Span::dummy(),
    );
    accepts_shared_statement(&statement);
}
