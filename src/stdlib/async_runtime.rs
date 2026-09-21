use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeIntrinsic, NativeRegistry};

fn promise_of(inner: SparType) -> SparType {
    SparType::Applied {
        name: "Promise".into(),
        arguments: vec![inner],
    }
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    let t = SparType::TypeParameter("T".into());
    registry
        .register(NativeFunction::intrinsic(
            "nativeAsync",
            "race",
            vec![("promises", SparType::List(Box::new(promise_of(t.clone()))))],
            t.clone(),
            true,
            NativeIntrinsic::PromiseRace,
        ))
        .expect("nativeAsync::race registration must be unique");

    registry
        .register(NativeFunction::intrinsic(
            "nativeAsync",
            "timeout",
            vec![
                ("promise", promise_of(t.clone())),
                ("millis", SparType::Int),
            ],
            t,
            true,
            NativeIntrinsic::PromiseTimeout,
        ))
        .expect("nativeAsync::timeout registration must be unique");
}
