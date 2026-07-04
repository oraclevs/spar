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
        is_private: false,
        span: Span::dummy(),
    };
    let _ = _fd;
}
