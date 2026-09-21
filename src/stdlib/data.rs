use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeIntrinsic, NativeMethod, NativeRegistry};

fn ty(name: &str) -> SparType {
    SparType::TypeParameter(name.into())
}

fn sequence(inner: SparType) -> SparType {
    SparType::Applied {
        name: "Sequence".into(),
        arguments: vec![inner],
    }
}

fn lookup(key: SparType, value: SparType) -> SparType {
    SparType::Applied {
        name: "Lookup".into(),
        arguments: vec![key, value],
    }
}

fn table(inner: SparType) -> SparType {
    SparType::Applied {
        name: "Table".into(),
        arguments: vec![inner],
    }
}

fn stream(inner: SparType) -> SparType {
    SparType::Applied {
        name: "Stream".into(),
        arguments: vec![inner],
    }
}

fn map_type(key: SparType, value: SparType) -> SparType {
    SparType::Applied {
        name: "Map".into(),
        arguments: vec![key, value],
    }
}

fn callable(input: SparType, output: SparType) -> SparType {
    SparType::Function {
        params: vec![input],
        return_type: Box::new(output),
    }
}

/// Every function `std/data` exports. Used for the missing-import hint and for
/// the interactive prelude; a test keeps it in step with `register`.
pub(crate) const FUNCTION_NAMES: &[&str] = &[
    "map",
    "filter",
    "where",
    "take",
    "skip",
    "first",
    "last",
    "collect",
    "collectTable",
    "count",
    "sortBy",
    "groupBy",
    "unique",
    "uniqueBy",
    "flatten",
    "get",
    "select",
    "schema",
    "inspect",
];

pub(crate) fn register(registry: &mut NativeRegistry) {
    let t = ty("T");
    let u = ty("U");
    let k = ty("K");
    let seq_t = sequence(t.clone());

    register_function(
        registry,
        "map",
        vec![
            ("source", seq_t.clone()),
            ("transform", callable(t.clone(), u.clone())),
        ],
        sequence(u.clone()),
        NativeIntrinsic::DataMap,
    );
    register_function(
        registry,
        "filter",
        vec![
            ("source", seq_t.clone()),
            ("predicate", callable(t.clone(), SparType::Bool)),
        ],
        seq_t.clone(),
        NativeIntrinsic::DataFilter,
    );
    register_function(
        registry,
        "where",
        vec![
            ("source", seq_t.clone()),
            ("predicate", callable(t.clone(), SparType::Bool)),
        ],
        seq_t.clone(),
        NativeIntrinsic::DataFilter,
    );
    register_function(
        registry,
        "take",
        vec![("source", seq_t.clone()), ("count", SparType::Int)],
        seq_t.clone(),
        NativeIntrinsic::DataTake,
    );
    register_function(
        registry,
        "skip",
        vec![("source", seq_t.clone()), ("count", SparType::Int)],
        seq_t.clone(),
        NativeIntrinsic::DataSkip,
    );
    register_function(
        registry,
        "first",
        vec![("source", seq_t.clone())],
        t.clone(),
        NativeIntrinsic::DataFirst,
    );
    register_function(
        registry,
        "last",
        vec![("source", seq_t.clone())],
        t.clone(),
        NativeIntrinsic::DataLast,
    );
    register_function(
        registry,
        "collect",
        vec![("source", seq_t.clone())],
        SparType::List(Box::new(t.clone())),
        NativeIntrinsic::DataCollect,
    );
    register_function(
        registry,
        "collectTable",
        vec![("source", seq_t.clone())],
        table(t.clone()),
        NativeIntrinsic::DataCollectTable,
    );
    register_function(
        registry,
        "count",
        vec![("source", seq_t.clone())],
        SparType::Int,
        NativeIntrinsic::DataCount,
    );
    register_function(
        registry,
        "sortBy",
        vec![
            ("source", seq_t.clone()),
            ("key", callable(t.clone(), k.clone())),
        ],
        seq_t.clone(),
        NativeIntrinsic::DataSortBy,
    );
    register_function(
        registry,
        "groupBy",
        vec![
            ("source", seq_t.clone()),
            ("key", callable(t.clone(), k.clone())),
        ],
        map_type(k.clone(), table(t.clone())),
        NativeIntrinsic::DataGroupBy,
    );
    register_function(
        registry,
        "unique",
        vec![("source", seq_t.clone())],
        seq_t.clone(),
        NativeIntrinsic::DataUnique,
    );
    register_function(
        registry,
        "uniqueBy",
        vec![
            ("source", seq_t.clone()),
            ("key", callable(t.clone(), k.clone())),
        ],
        seq_t.clone(),
        NativeIntrinsic::DataUniqueBy,
    );
    register_function(
        registry,
        "flatten",
        vec![("source", sequence(SparType::List(Box::new(t.clone()))))],
        seq_t.clone(),
        NativeIntrinsic::DataFlatten,
    );
    register_function(
        registry,
        "get",
        vec![("source", lookup(k.clone(), t.clone())), ("key", k.clone())],
        t.clone(),
        NativeIntrinsic::DataGet,
    );
    register_function(
        registry,
        "select",
        vec![
            ("source", seq_t.clone()),
            ("fields", SparType::List(Box::new(SparType::Str))),
        ],
        sequence(SparType::Named("Record".into())),
        NativeIntrinsic::DataSelect,
    );
    register_function(
        registry,
        "schema",
        vec![("source", seq_t.clone())],
        SparType::Named("Schema".into()),
        NativeIntrinsic::DataSchema,
    );
    register_function(
        registry,
        "inspect",
        vec![("source", seq_t.clone())],
        seq_t,
        NativeIntrinsic::DataInspect,
    );

    register_sequence_methods(
        registry,
        "List",
        SparType::List(Box::new(t.clone())),
        t.clone(),
    );
    register_sequence_methods(registry, "Table", table(t.clone()), t.clone());
    register_sequence_methods(registry, "Stream", stream(t.clone()), t.clone());

    registry
        .register_method(NativeMethod::intrinsic(
            "Map",
            "get",
            map_type(k.clone(), t.clone()),
            vec![("key", k)],
            t,
            false,
            NativeIntrinsic::DataGet,
        ))
        .expect("Map.get registration must be unique");
}

fn register_function(
    registry: &mut NativeRegistry,
    name: &str,
    params: Vec<(&str, SparType)>,
    ret: SparType,
    intrinsic: NativeIntrinsic,
) {
    registry
        .register(NativeFunction::sync_intrinsic(
            "nativeData",
            name,
            params,
            ret,
            true,
            intrinsic,
        ))
        .unwrap_or_else(|_| panic!("nativeData::{name} registration must be unique"));
}

fn register_sequence_methods(
    registry: &mut NativeRegistry,
    owner: &str,
    receiver: SparType,
    element: SparType,
) {
    let u = ty("U");
    let k = ty("K");
    let same = receiver.clone();
    let mapped = match owner {
        "List" => SparType::List(Box::new(u.clone())),
        "Table" => table(u.clone()),
        "Stream" => stream(u.clone()),
        _ => unreachable!(),
    };
    let selected = match owner {
        "List" => SparType::List(Box::new(SparType::Named("Record".into()))),
        "Table" => table(SparType::Named("Record".into())),
        "Stream" => stream(SparType::Named("Record".into())),
        _ => unreachable!(),
    };

    let methods = vec![
        NativeMethod::intrinsic(
            owner,
            "map",
            receiver.clone(),
            vec![("transform", callable(element.clone(), u))],
            mapped,
            false,
            NativeIntrinsic::DataMap,
        ),
        NativeMethod::intrinsic(
            owner,
            "filter",
            receiver.clone(),
            vec![("predicate", callable(element.clone(), SparType::Bool))],
            same.clone(),
            false,
            NativeIntrinsic::DataFilter,
        ),
        NativeMethod::intrinsic(
            owner,
            "where",
            receiver.clone(),
            vec![("predicate", callable(element.clone(), SparType::Bool))],
            same.clone(),
            false,
            NativeIntrinsic::DataFilter,
        ),
        NativeMethod::intrinsic(
            owner,
            "take",
            receiver.clone(),
            vec![("count", SparType::Int)],
            same.clone(),
            false,
            NativeIntrinsic::DataTake,
        ),
        NativeMethod::intrinsic(
            owner,
            "skip",
            receiver.clone(),
            vec![("count", SparType::Int)],
            same.clone(),
            false,
            NativeIntrinsic::DataSkip,
        ),
        NativeMethod::intrinsic(
            owner,
            "first",
            receiver.clone(),
            vec![],
            element.clone(),
            false,
            NativeIntrinsic::DataFirst,
        ),
        NativeMethod::intrinsic(
            owner,
            "last",
            receiver.clone(),
            vec![],
            element.clone(),
            false,
            NativeIntrinsic::DataLast,
        ),
        NativeMethod::intrinsic(
            owner,
            "collect",
            receiver.clone(),
            vec![],
            SparType::List(Box::new(element.clone())),
            false,
            NativeIntrinsic::DataCollect,
        ),
        NativeMethod::intrinsic(
            owner,
            "collectTable",
            receiver.clone(),
            vec![],
            table(element.clone()),
            false,
            NativeIntrinsic::DataCollectTable,
        ),
        NativeMethod::intrinsic(
            owner,
            "count",
            receiver.clone(),
            vec![],
            SparType::Int,
            false,
            NativeIntrinsic::DataCount,
        ),
        NativeMethod::intrinsic(
            owner,
            "sortBy",
            receiver.clone(),
            vec![("key", callable(element.clone(), k.clone()))],
            same.clone(),
            false,
            NativeIntrinsic::DataSortBy,
        ),
        NativeMethod::intrinsic(
            owner,
            "groupBy",
            receiver.clone(),
            vec![("key", callable(element.clone(), k.clone()))],
            map_type(k.clone(), table(element.clone())),
            false,
            NativeIntrinsic::DataGroupBy,
        ),
        NativeMethod::intrinsic(
            owner,
            "unique",
            receiver.clone(),
            vec![],
            same.clone(),
            false,
            NativeIntrinsic::DataUnique,
        ),
        NativeMethod::intrinsic(
            owner,
            "uniqueBy",
            receiver.clone(),
            vec![("key", callable(element.clone(), k))],
            same.clone(),
            false,
            NativeIntrinsic::DataUniqueBy,
        ),
        NativeMethod::intrinsic(
            owner,
            "get",
            receiver.clone(),
            vec![("key", SparType::Int)],
            element.clone(),
            false,
            NativeIntrinsic::DataGet,
        ),
        NativeMethod::intrinsic(
            owner,
            "select",
            receiver.clone(),
            vec![("fields", SparType::List(Box::new(SparType::Str)))],
            selected,
            false,
            NativeIntrinsic::DataSelect,
        ),
        NativeMethod::intrinsic(
            owner,
            "schema",
            receiver.clone(),
            vec![],
            SparType::Named("Schema".into()),
            false,
            NativeIntrinsic::DataSchema,
        ),
        NativeMethod::intrinsic(
            owner,
            "inspect",
            receiver,
            vec![],
            same,
            false,
            NativeIntrinsic::DataInspect,
        ),
    ];

    if matches!(owner, "List" | "Stream") {
        let flattened_element = ty("F");
        let nested_receiver = match owner {
            "List" => SparType::List(Box::new(SparType::List(Box::new(
                flattened_element.clone(),
            )))),
            "Stream" => stream(SparType::List(Box::new(flattened_element.clone()))),
            _ => unreachable!(),
        };
        let flattened = match owner {
            "List" => SparType::List(Box::new(flattened_element.clone())),
            "Stream" => stream(flattened_element),
            _ => unreachable!(),
        };
        let method = NativeMethod::intrinsic(
            owner,
            "flatten",
            nested_receiver,
            vec![],
            flattened,
            false,
            NativeIntrinsic::DataFlatten,
        );
        registry
            .register_method(method)
            .unwrap_or_else(|_| panic!("{owner}.flatten registration must be unique"));
    }

    for method in methods {
        let name = method.name.clone();
        registry
            .register_method(method)
            .unwrap_or_else(|_| panic!("{owner}.{name} registration must be unique"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_and_stream_methods_route_to_the_same_intrinsics_as_free_functions() {
        let mut registry = NativeRegistry::new();
        register(&mut registry);

        for (method, intrinsic) in [
            ("map", NativeIntrinsic::DataMap),
            ("filter", NativeIntrinsic::DataFilter),
            ("take", NativeIntrinsic::DataTake),
            ("collect", NativeIntrinsic::DataCollect),
            ("groupBy", NativeIntrinsic::DataGroupBy),
            ("select", NativeIntrinsic::DataSelect),
            ("inspect", NativeIntrinsic::DataInspect),
        ] {
            let (function_id, _) = registry
                .get("nativeData", method)
                .unwrap_or_else(|| panic!("missing nativeData::{method}"));
            assert_eq!(registry.intrinsic(function_id), Some(intrinsic));

            for owner in ["Table", "Stream"] {
                let signature = registry
                    .method_signature(owner, method)
                    .unwrap_or_else(|| panic!("missing {owner}.{method}"));
                assert_eq!(registry.method_intrinsic(signature.id), Some(intrinsic));
            }
        }
    }

    #[test]
    fn function_names_match_the_registered_functions() {
        let source = include_str!("data.rs");
        let registered = source
            .matches("register_function(\n        registry,\n        \"")
            .count();
        assert_eq!(registered, super::FUNCTION_NAMES.len());
        for name in super::FUNCTION_NAMES {
            assert!(
                source.contains(&format!(
                    "register_function(\n        registry,\n        \"{name}\","
                )),
                "`{name}` is listed but not registered"
            );
        }
    }
}
