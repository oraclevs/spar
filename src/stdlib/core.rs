use crate::runtime::value::Shared;
use crate::ast::{CallableParamType, SparType};
use crate::runtime::{NativeFunction, NativeMethod, NativeRegistry, Value};

use super::support::{error, int_arg};

fn type_parameter(name: &str) -> SparType {
    SparType::TypeParameter(name.into())
}

fn applied(name: &str, arguments: Vec<SparType>) -> SparType {
    SparType::Applied {
        name: name.into(),
        arguments,
    }
}

fn callable(params: Vec<(&str, SparType)>, ret: SparType) -> SparType {
    SparType::Function {
        params: params
            .into_iter()
            .map(|(name, ty)| CallableParamType {
                name: name.into(),
                ty,
            })
            .collect(),
        return_type: Box::new(ret),
    }
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "len",
            vec![("value", SparType::TypeParameter("T".into()))],
            SparType::Int,
            true,
            |_context, args| length_value(args),
        ))
        .expect("nativeCore::len registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "range",
            vec![("start", SparType::Int), ("end", SparType::Int)],
            SparType::List(Box::new(SparType::Int)),
            true,
            |_context, args| {
                let start = int_arg(args, 0, "start")?;
                let end = int_arg(args, 1, "end")?;
                Ok(Value::List((start..end).map(Value::Int).collect()))
            },
        ))
        .expect("nativeCore::range registration must be unique");

    register_sum_type_constructors(registry);
    register_universal_methods(registry);
    register_collection_methods(registry);
    register_primitive_methods(registry);
    register_record_methods(registry);
    register_option_methods(registry);
    register_result_methods(registry);
    register_table_methods(registry);
}

fn register_universal_methods(registry: &mut NativeRegistry) {
    registry
        .register_method(NativeMethod::sync(
            "Any",
            "toString",
            SparType::Any,
            vec![],
            SparType::Str,
            false,
            |_context, args| {
                let value = args
                    .first()
                    .ok_or_else(|| error("Any.toString receiver is missing"))?;
                Ok(Value::String(value.render_display()))
            },
        ))
        .expect("Any.toString registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Any",
            "typeName",
            SparType::Any,
            vec![],
            SparType::Str,
            false,
            |_context, args| {
                let value = args
                    .first()
                    .ok_or_else(|| error("Any.typeName receiver is missing"))?;
                Ok(Value::String(value.type_name().to_string()))
            },
        ))
        .expect("Any.typeName registration must be unique");
}

fn register_sum_type_constructors(registry: &mut NativeRegistry) {
    let t = type_parameter("T");
    let e = type_parameter("E");
    let option_t = applied("Option", vec![t.clone()]);
    let result_te = applied("Result", vec![t.clone(), e.clone()]);

    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "some",
            vec![("value", t.clone())],
            option_t.clone(),
            true,
            |_context, args| {
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| error("missing native argument 'value'"))?;
                Ok(Value::Option(Some(Box::new(value))))
            },
        ))
        .expect("nativeCore::some registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "none",
            vec![],
            option_t,
            true,
            |_context, _args| Ok(Value::Option(None)),
        ))
        .expect("nativeCore::none registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "ok",
            vec![("value", t)],
            result_te.clone(),
            true,
            |_context, args| {
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| error("missing native argument 'value'"))?;
                Ok(Value::Result(Ok(Box::new(value))))
            },
        ))
        .expect("nativeCore::ok registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "err",
            vec![("error", e), ("exitCode", SparType::Int)],
            result_te,
            true,
            |_context, args| {
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| error("missing native argument 'error'"))?;
                let code = match args.get(1) {
                    Some(Value::Int(code)) if (0..=255).contains(code) => *code as i32,
                    Some(Value::Int(code)) => {
                        return Err(error(format!(
                            "err exitCode must be between 0 and 255, found {code}"
                        )))
                    }
                    _ => return Err(error("err exitCode must be an int")),
                };
                Ok(Value::Result(Err(crate::runtime::value::ErrBox::with_exit_code(value, code))))
            },
        ))
        .expect("nativeCore::err registration must be unique");
}

fn register_collection_methods(registry: &mut NativeRegistry) {
    registry
        .register_method(NativeMethod::sync(
            "str",
            "length",
            SparType::Str,
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("str.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "utf8ByteLength",
            SparType::Str,
            vec![],
            SparType::Int,
            false,
            |_context, args| Ok(Value::Int(string_receiver(args)?.len() as i64)),
        ))
        .expect("str.utf8ByteLength registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "isEmpty",
            SparType::Str,
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("str.isEmpty registration must be unique");

    for method in [
        NativeMethod::sync(
            "str", "contains", SparType::Str, vec![("needle", SparType::Str)], SparType::Bool, false,
            |_context, args| Ok(Value::Bool(string_receiver(args)?.contains(string_arg(args, 1, "str.contains", "needle")?))),
        ),
        NativeMethod::sync(
            "str", "startsWith", SparType::Str, vec![("prefix", SparType::Str)], SparType::Bool, false,
            |_context, args| Ok(Value::Bool(string_receiver(args)?.starts_with(string_arg(args, 1, "str.startsWith", "prefix")?))),
        ),
        NativeMethod::sync(
            "str", "endsWith", SparType::Str, vec![("suffix", SparType::Str)], SparType::Bool, false,
            |_context, args| Ok(Value::Bool(string_receiver(args)?.ends_with(string_arg(args, 1, "str.endsWith", "suffix")?))),
        ),
        NativeMethod::sync(
            "str", "trim", SparType::Str, vec![], SparType::Str, false,
            |_context, args| Ok(Value::String(string_receiver(args)?.trim().to_string())),
        ),
        NativeMethod::sync(
            "str", "trimStart", SparType::Str, vec![], SparType::Str, false,
            |_context, args| Ok(Value::String(string_receiver(args)?.trim_start().to_string())),
        ),
        NativeMethod::sync(
            "str", "trimEnd", SparType::Str, vec![], SparType::Str, false,
            |_context, args| Ok(Value::String(string_receiver(args)?.trim_end().to_string())),
        ),
        NativeMethod::sync(
            "str", "toLowerCase", SparType::Str, vec![], SparType::Str, false,
            |_context, args| Ok(Value::String(string_receiver(args)?.to_lowercase())),
        ),
        NativeMethod::sync(
            "str", "toUpperCase", SparType::Str, vec![], SparType::Str, false,
            |_context, args| Ok(Value::String(string_receiver(args)?.to_uppercase())),
        ),
        NativeMethod::sync(
            "str", "split", SparType::Str, vec![("separator", SparType::Str)], SparType::List(Box::new(SparType::Str)), false,
            |_context, args| {
                let source = string_receiver(args)?;
                let separator = string_arg(args, 1, "str.split", "separator")?;
                Ok(Value::List(source.split(separator).map(|part| Value::String(part.to_string())).collect()))
            },
        ),
        NativeMethod::sync(
            "str", "replace", SparType::Str, vec![("from", SparType::Str), ("to", SparType::Str)], SparType::Str, false,
            |_context, args| {
                let source = string_receiver(args)?;
                let from = string_arg(args, 1, "str.replace", "from")?;
                let to = string_arg(args, 2, "str.replace", "to")?;
                Ok(Value::String(source.replace(from, to)))
            },
        ),
        NativeMethod::sync(
            "str", "substring", SparType::Str,
            vec![("start", SparType::Int), ("end", applied("Option", vec![SparType::Int]))],
            SparType::Str, false,
            |_context, args| substring_value(args),
        ),
        NativeMethod::sync(
            "str", "indexOf", SparType::Str, vec![("needle", SparType::Str)], applied("Option", vec![SparType::Int]), false,
            |_context, args| string_index_of(args),
        ),
    ] {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("str.{name} registration must be unique"));
    }

    let t = type_parameter("T");
    let list_t = SparType::List(Box::new(t.clone()));
    registry
        .register_method(NativeMethod::sync(
            "List",
            "length",
            list_t.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("List.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "List",
            "isEmpty",
            list_t.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("List.isEmpty registration must be unique");


    let option_t = applied("Option", vec![t.clone()]);
    for method in [
        NativeMethod::sync(
            "List", "contains", list_t.clone(), vec![("value", t.clone())], SparType::Bool, false,
            |_context, args| {
                let receiver = list_receiver(args)?;
                let value = args.get(1).ok_or_else(|| error("missing List.contains value"))?;
                Ok(Value::Bool(receiver.iter().any(|item| item == value)))
            },
        ),
        NativeMethod::sync(
            "List", "get", list_t.clone(), vec![("index", SparType::Int)], option_t.clone(), false,
            |_context, args| {
                let receiver = list_receiver(args)?;
                let index = non_negative_index(args.get(1), "List.get")?;
                Ok(Value::Option(receiver.get(index).cloned().map(Box::new)))
            },
        ),
        NativeMethod::sync(
            "List", "first", list_t.clone(), vec![], option_t.clone(), false,
            |_context, args| Ok(Value::Option(list_receiver(args)?.first().cloned().map(Box::new))),
        ),
        NativeMethod::sync(
            "List", "last", list_t.clone(), vec![], option_t.clone(), false,
            |_context, args| Ok(Value::Option(list_receiver(args)?.last().cloned().map(Box::new))),
        ),
        NativeMethod::sync(
            "List", "indexOf", list_t.clone(), vec![("value", t.clone())], applied("Option", vec![SparType::Int]), false,
            |_context, args| {
                let receiver = list_receiver(args)?;
                let value = args.get(1).ok_or_else(|| error("missing List.indexOf value"))?;
                let index = receiver.iter().position(|item| item == value).and_then(|index| i64::try_from(index).ok());
                Ok(Value::Option(index.map(|index| Box::new(Value::Int(index)))))
            },
        ),
        NativeMethod::sync(
            "List", "reversed", list_t.clone(), vec![], list_t.clone(), false,
            |_context, args| {
                let mut values = list_receiver(args)?.to_vec();
                values.reverse();
                Ok(Value::List(Shared::from(values)))
            },
        ),
    ] {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("List.{name} registration must be unique"));
    }

    let u = type_parameter("U");
    for method in [
        NativeMethod::intrinsic(
            "List", "map", list_t.clone(),
            vec![("transform", callable(vec![("value", t.clone())], u.clone()))],
            SparType::List(Box::new(u)), false,
            crate::runtime::NativeIntrinsic::DataMap,
        ),
        NativeMethod::intrinsic(
            "List", "filter", list_t.clone(),
            vec![("predicate", callable(vec![("value", t.clone())], SparType::Bool))],
            list_t.clone(), false,
            crate::runtime::NativeIntrinsic::DataFilter,
        ),
        NativeMethod::intrinsic(
            "List", "take", list_t.clone(), vec![("count", SparType::Int)], list_t.clone(), false,
            crate::runtime::NativeIntrinsic::DataTake,
        ),
        NativeMethod::intrinsic(
            "List", "skip", list_t.clone(), vec![("count", SparType::Int)], list_t.clone(), false,
            crate::runtime::NativeIntrinsic::DataSkip,
        ),
        NativeMethod::intrinsic(
            "List", "find", list_t.clone(),
            vec![("predicate", callable(vec![("value", t.clone())], SparType::Bool))],
            option_t.clone(), false,
            crate::runtime::NativeIntrinsic::DataFind,
        ),
        NativeMethod::intrinsic(
            "List", "findIndex", list_t.clone(),
            vec![("predicate", callable(vec![("value", t.clone())], SparType::Bool))],
            applied("Option", vec![SparType::Int]), false,
            crate::runtime::NativeIntrinsic::DataFindIndex,
        ),
        NativeMethod::intrinsic(
            "List", "any", list_t.clone(),
            vec![("predicate", callable(vec![("value", t.clone())], SparType::Bool))],
            SparType::Bool, false,
            crate::runtime::NativeIntrinsic::DataAny,
        ),
        NativeMethod::intrinsic(
            "List", "every", list_t.clone(),
            vec![("predicate", callable(vec![("value", t.clone())], SparType::Bool))],
            SparType::Bool, false,
            crate::runtime::NativeIntrinsic::DataEvery,
        ),
    ] {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("List.{name} registration must be unique"));
    }

    registry
        .register_method(NativeMethod::sync(
            "List",
            "join",
            SparType::List(Box::new(SparType::Str)),
            vec![("separator", SparType::Str)],
            SparType::Str,
            false,
            |_context, args| {
                let separator = string_arg(args, 1, "List.join", "separator")?;
                let values = list_receiver(args)?;
                let mut parts = Vec::with_capacity(values.len());
                for value in values {
                    let Value::String(value) = value else {
                        return Err(error("List.join receiver must be List<str>"));
                    };
                    parts.push(value.as_str());
                }
                Ok(Value::String(parts.join(separator)))
            },
        ))
        .expect("List.join registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "List",
            "asArgs",
            SparType::List(Box::new(SparType::Str)),
            vec![],
            SparType::Named("Args".into()),
            false,
            |_context, args| {
                let values = list_receiver(args)?;
                let mut argv = Vec::with_capacity(values.len());
                for value in values {
                    let Value::String(value) = value else {
                        return Err(error("List.asArgs receiver must be List<str>"));
                    };
                    argv.push(value.clone());
                }
                Ok(Value::Args(argv))
            },
        ))
        .expect("List<str>.asArgs registration must be unique");

    for method in [
        NativeMethod::sync_mut(
            "List", "append", list_t.clone(), vec![("value", t.clone())], SparType::Void, false,
            |_context, receiver, args| {
                let Value::List(values) = receiver else { return Err(error("List.append receiver is not a list")); };
                values.push(args.first().cloned().ok_or_else(|| error("missing List.append value"))?);
                Ok(Value::Void)
            },
        ),
        NativeMethod::sync_mut(
            "List", "add", list_t.clone(), vec![("value", t.clone())], SparType::Void, false,
            |_context, receiver, args| {
                let Value::List(values) = receiver else { return Err(error("List.add receiver is not a list")); };
                values.push(args.first().cloned().ok_or_else(|| error("missing List.add value"))?);
                Ok(Value::Void)
            },
        ),
        NativeMethod::sync_mut(
            "List", "appendAll", list_t.clone(), vec![("values", list_t.clone())], SparType::Void, false,
            |_context, receiver, args| {
                let incoming = match args.first() { Some(Value::List(values)) => values.clone(), _ => return Err(error("List.appendAll values must be a list")) };
                let Value::List(values) = receiver else { return Err(error("List.appendAll receiver is not a list")); };
                values.extend(incoming);
                Ok(Value::Void)
            },
        ),
        NativeMethod::sync_mut(
            "List", "insert", list_t.clone(), vec![("index", SparType::Int), ("value", t.clone())], SparType::Void, false,
            |_context, receiver, args| {
                let index = non_negative_index(args.first(), "List.insert")?;
                let value = args.get(1).cloned().ok_or_else(|| error("missing List.insert value"))?;
                let Value::List(values) = receiver else { return Err(error("List.insert receiver is not a list")); };
                if index > values.len() { return Err(error("List.insert index is out of bounds")); }
                values.insert(index, value);
                Ok(Value::Void)
            },
        ),
        NativeMethod::sync_mut(
            "List", "insertAll", list_t.clone(), vec![("index", SparType::Int), ("values", list_t.clone())], SparType::Void, false,
            |_context, receiver, args| {
                let index = non_negative_index(args.first(), "List.insertAll")?;
                let incoming = match args.get(1) { Some(Value::List(values)) => values.clone(), _ => return Err(error("List.insertAll values must be a list")) };
                let Value::List(values) = receiver else { return Err(error("List.insertAll receiver is not a list")); };
                if index > values.len() { return Err(error("List.insertAll index is out of bounds")); }
                values.splice(index..index, incoming);
                Ok(Value::Void)
            },
        ),
        NativeMethod::sync_mut(
            "List", "set", list_t.clone(), vec![("index", SparType::Int), ("value", t.clone())], SparType::Void, false,
            |_context, receiver, args| {
                let index = non_negative_index(args.first(), "List.set")?;
                let value = args.get(1).cloned().ok_or_else(|| error("missing List.set value"))?;
                let Value::List(values) = receiver else { return Err(error("List.set receiver is not a list")); };
                let slot = values.get_mut(index).ok_or_else(|| error("List.set index is out of bounds"))?;
                *slot = value;
                Ok(Value::Void)
            },
        ),
        NativeMethod::sync_mut(
            "List", "remove", list_t.clone(), vec![("value", t.clone())], SparType::Bool, false,
            |_context, receiver, args| {
                let needle = args.first().ok_or_else(|| error("missing List.remove value"))?;
                let Value::List(values) = receiver else { return Err(error("List.remove receiver is not a list")); };
                if let Some(index) = values.iter().position(|item| item == needle) { values.remove(index); Ok(Value::Bool(true)) } else { Ok(Value::Bool(false)) }
            },
        ),
        NativeMethod::sync_mut(
            "List", "removeAt", list_t.clone(), vec![("index", SparType::Int)], t.clone(), false,
            |_context, receiver, args| {
                let index = non_negative_index(args.first(), "List.removeAt")?;
                let Value::List(values) = receiver else { return Err(error("List.removeAt receiver is not a list")); };
                if index >= values.len() {
                    return Err(error("List.removeAt index is out of bounds"));
                }
                Ok(values.remove(index))
            },
        ),
        NativeMethod::sync_mut(
            "List", "pop", list_t.clone(), vec![], option_t.clone(), false,
            |_context, receiver, _args| {
                let Value::List(values) = receiver else { return Err(error("List.pop receiver is not a list")); };
                Ok(Value::Option(values.pop().map(Box::new)))
            },
        ),
        NativeMethod::sync_mut(
            "List", "clear", list_t.clone(), vec![], SparType::Void, false,
            |_context, receiver, _args| {
                let Value::List(values) = receiver else { return Err(error("List.clear receiver is not a list")); };
                values.clear(); Ok(Value::Void)
            },
        ),
        NativeMethod::sync_mut(
            "List", "reverse", list_t.clone(), vec![], SparType::Void, false,
            |_context, receiver, _args| {
                let Value::List(values) = receiver else { return Err(error("List.reverse receiver is not a list")); };
                values.reverse(); Ok(Value::Void)
            },
        ),
    ] {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("List.{name} registration must be unique"));
    }

    let bytes = SparType::Named("Bytes".into());
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "length",
            bytes.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("Bytes.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "isEmpty",
            bytes.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("Bytes.isEmpty registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "concat",
            bytes.clone(),
            vec![("other", bytes.clone())],
            bytes.clone(),
            false,
            |_context, args| match (args.first(), args.get(1)) {
                (Some(Value::Bytes(left)), Some(Value::Bytes(right))) => {
                    let mut joined = Vec::with_capacity(left.len() + right.len());
                    joined.extend_from_slice(left);
                    joined.extend_from_slice(right);
                    Ok(Value::Bytes(joined))
                }
                _ => Err(error("Bytes.concat expects Bytes values")),
            },
        ))
        .expect("Bytes.concat registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "get",
            bytes.clone(),
            vec![("index", SparType::Int)],
            applied("Option", vec![SparType::Int]),
            false,
            |_context, args| {
                let index = non_negative_index(args.get(1), "Bytes.get")?;
                let Some(Value::Bytes(values)) = args.first() else {
                    return Err(error("Bytes.get receiver is not Bytes"));
                };
                Ok(Value::Option(
                    values
                        .get(index)
                        .map(|value| Box::new(Value::Int(i64::from(*value)))),
                ))
            },
        ))
        .expect("Bytes.get registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "slice",
            bytes.clone(),
            vec![("start", SparType::Int), ("end", applied("Option", vec![SparType::Int]))],
            bytes.clone(),
            false,
            |_context, args| {
                let start = non_negative_index(args.get(1), "Bytes.slice")?;
                let Some(Value::Bytes(values)) = args.first() else {
                    return Err(error("Bytes.slice receiver is not Bytes"));
                };
                let end = option_index(args.get(2), values.len(), "Bytes.slice")?;
                if start > end || end > values.len() {
                    return Err(error("Bytes.slice range is out of bounds"));
                }
                Ok(Value::Bytes(values[start..end].to_vec()))
            },
        ))
        .expect("Bytes.slice registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "toUtf8",
            bytes,
            vec![],
            applied("Result", vec![SparType::Str, SparType::Error]),
            false,
            |_context, args| {
                let Some(Value::Bytes(values)) = args.first() else {
                    return Err(error("Bytes.toUtf8 receiver is not Bytes"));
                };
                match String::from_utf8(values.clone()) {
                    Ok(value) => Ok(Value::Result(Ok(Box::new(Value::String(value))))),
                    Err(err) => Ok(Value::Result(Err(crate::runtime::value::ErrBox::new(Value::Error(Box::new(crate::runtime::value::ErrorValue {
                        message: err.to_string(),
                        kind: "Utf8Error".into(),
                        code: 0,
                        cause: None,
                    })))))),
                }
            },
        ))
        .expect("Bytes.toUtf8 registration must be unique");

    let k = type_parameter("K");
    let v = type_parameter("V");
    let map = applied("Map", vec![k.clone(), v.clone()]);
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "length",
            map.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("Map.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "isEmpty",
            map.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("Map.isEmpty registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "keys",
            map.clone(),
            vec![],
            SparType::List(Box::new(k.clone())),
            false,
            |_context, args| map_keys(args),
        ))
        .expect("Map.keys registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "values",
            map.clone(),
            vec![],
            SparType::List(Box::new(v.clone())),
            false,
            |_context, args| map_values(args),
        ))
        .expect("Map.values registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "containsKey",
            map.clone(),
            vec![("key", k.clone())],
            SparType::Bool,
            false,
            |_context, args| map_contains_key(args),
        ))
        .expect("Map.containsKey registration must be unique");


    let option_v = applied("Option", vec![v.clone()]);
    for method in [
        NativeMethod::sync(
            "Map", "get", map.clone(), vec![("key", k.clone())], option_v.clone(), false,
            |_context, args| {
                let key = args.get(1).ok_or_else(|| error("missing Map.get key"))?;
                Ok(Value::Option(map_receiver(args)?.get(key).cloned().map(Box::new)))
            },
        ),
        NativeMethod::sync(
            "Map", "containsValue", map.clone(), vec![("value", v.clone())], SparType::Bool, false,
            |_context, args| {
                let value = args.get(1).ok_or_else(|| error("missing Map.containsValue value"))?;
                Ok(Value::Bool(map_receiver(args)?.contains_value(value)))
            },
        ),
        NativeMethod::sync(
            "Map",
            "entries",
            map.clone(),
            vec![],
            SparType::List(Box::new(applied("MapEntry", vec![k.clone(), v.clone()]))),
            false,
            |_context, args| {
                Ok(Value::List(
                    map_receiver(args)?
                        .iter()
                        .map(|(key, value)| {
                            Value::Object(Shared::from(indexmap::IndexMap::from([
                                ("key".into(), key),
                                ("value".into(), value.clone()),
                            ])))
                        })
                        .collect(),
                ))
            },
        ),
        NativeMethod::sync(
            "Map", "getOr", map.clone(), vec![("key", k.clone()), ("fallback", v.clone())], v.clone(), false,
            |_context, args| {
                let key = args.get(1).ok_or_else(|| error("missing Map.getOr key"))?;
                let fallback = args.get(2).cloned().ok_or_else(|| error("missing Map.getOr fallback"))?;
                Ok(map_receiver(args)?.get(key).cloned().unwrap_or(fallback))
            },
        ),
        NativeMethod::intrinsic(
            "Map",
            "getOrElse",
            map.clone(),
            vec![("key", k.clone()), ("fallback", callable(vec![], v.clone()))],
            v.clone(),
            false,
            crate::runtime::NativeIntrinsic::CoreMapGetOrElse,
        ),
    ] {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("Map.{name} registration must be unique"));
    }
    for method in [
        NativeMethod::sync_mut(
            "Map", "insert", map.clone(), vec![("key", k.clone()), ("value", v.clone())], option_v.clone(), false,
            |_context, receiver, args| {
                let key = args.first().cloned().ok_or_else(|| error("missing Map.insert key"))?;
                let value = args.get(1).cloned().ok_or_else(|| error("missing Map.insert value"))?;
                let Value::Map(entries) = receiver else { return Err(error("Map.insert receiver is not a map")); };
                Ok(Value::Option(entries.insert(key, value).map(Box::new)))
            },
        ),
        NativeMethod::sync_mut(
            "Map", "remove", map.clone(), vec![("key", k.clone())], option_v.clone(), false,
            |_context, receiver, args| {
                let key = args.first().ok_or_else(|| error("missing Map.remove key"))?;
                let Value::Map(entries) = receiver else { return Err(error("Map.remove receiver is not a map")); };
                Ok(Value::Option(entries.remove(key).map(Box::new)))
            },
        ),
        NativeMethod::sync_mut(
            "Map", "clear", map.clone(), vec![], SparType::Void, false,
            |_context, receiver, _args| {
                let Value::Map(entries) = receiver else { return Err(error("Map.clear receiver is not a map")); };
                entries.clear(); Ok(Value::Void)
            },
        ),
    ] {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("Map.{name} registration must be unique"));
    }
}

fn register_primitive_methods(registry: &mut NativeRegistry) {
    for method in [
        NativeMethod::sync(
            "int", "abs", SparType::Int, vec![], SparType::Int, false,
            |_context, args| match args.first() {
                Some(Value::Int(value)) => value
                    .checked_abs()
                    .map(Value::Int)
                    .ok_or_else(|| error("integer overflow in abs")),
                _ => Err(error("int.abs receiver is not int")),
            },
        ),
        // Explicit overflow behaviour. The `+ - * / -` operators are checked
        // (overflow is a runtime error); these opt in to the other semantics.
        NativeMethod::sync(
            "int", "wrappingAdd", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int(a.wrapping_add(*b))), _ => Err(error("int.wrappingAdd expects int values")) },
        ),
        NativeMethod::sync(
            "int", "wrappingSub", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int(a.wrapping_sub(*b))), _ => Err(error("int.wrappingSub expects int values")) },
        ),
        NativeMethod::sync(
            "int", "wrappingMul", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int(a.wrapping_mul(*b))), _ => Err(error("int.wrappingMul expects int values")) },
        ),
        NativeMethod::sync(
            "int", "wrappingNeg", SparType::Int, vec![], SparType::Int, false,
            |_context, args| match args.first() { Some(Value::Int(a)) => Ok(Value::Int(a.wrapping_neg())), _ => Err(error("int.wrappingNeg receiver is not int")) },
        ),
        NativeMethod::sync(
            "int", "saturatingAdd", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int(a.saturating_add(*b))), _ => Err(error("int.saturatingAdd expects int values")) },
        ),
        NativeMethod::sync(
            "int", "saturatingSub", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int(a.saturating_sub(*b))), _ => Err(error("int.saturatingSub expects int values")) },
        ),
        NativeMethod::sync(
            "int", "saturatingMul", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int(a.saturating_mul(*b))), _ => Err(error("int.saturatingMul expects int values")) },
        ),
        NativeMethod::sync(
            "int", "checkedAdd", SparType::Int, vec![("other", SparType::Int)], applied("Option", vec![SparType::Int]), false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Option(a.checked_add(*b).map(|v| Box::new(Value::Int(v))))), _ => Err(error("int.checkedAdd expects int values")) },
        ),
        NativeMethod::sync(
            "int", "checkedSub", SparType::Int, vec![("other", SparType::Int)], applied("Option", vec![SparType::Int]), false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Option(a.checked_sub(*b).map(|v| Box::new(Value::Int(v))))), _ => Err(error("int.checkedSub expects int values")) },
        ),
        NativeMethod::sync(
            "int", "checkedMul", SparType::Int, vec![("other", SparType::Int)], applied("Option", vec![SparType::Int]), false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Option(a.checked_mul(*b).map(|v| Box::new(Value::Int(v))))), _ => Err(error("int.checkedMul expects int values")) },
        ),
        NativeMethod::sync(
            "int", "min", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int((*a).min(*b))), _ => Err(error("int.min expects int values")) },
        ),
        NativeMethod::sync(
            "int", "max", SparType::Int, vec![("other", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Int(a)), Some(Value::Int(b))) => Ok(Value::Int((*a).max(*b))), _ => Err(error("int.max expects int values")) },
        ),
        NativeMethod::sync(
            "int", "clamp", SparType::Int, vec![("min", SparType::Int), ("max", SparType::Int)], SparType::Int, false,
            |_context, args| match (args.first(), args.get(1), args.get(2)) {
                (Some(Value::Int(value)), Some(Value::Int(min)), Some(Value::Int(max))) if min <= max => Ok(Value::Int((*value).clamp(*min, *max))),
                (Some(Value::Int(_)), Some(Value::Int(_)), Some(Value::Int(_))) => Err(error("int.clamp min cannot exceed max")),
                _ => Err(error("int.clamp expects int values")),
            },
        ),
        NativeMethod::sync(
            "int", "toFloat", SparType::Int, vec![], SparType::Float, false,
            |_context, args| match args.first() { Some(Value::Int(value)) => Ok(Value::Float(*value as f64)), _ => Err(error("int.toFloat receiver is not int")) },
        ),
        NativeMethod::sync(
            "int", "toString", SparType::Int, vec![], SparType::Str, false,
            |_context, args| match args.first() { Some(Value::Int(value)) => Ok(Value::String(value.to_string())), _ => Err(error("int.toString receiver is not int")) },
        ),
        NativeMethod::sync(
            "int", "isEven", SparType::Int, vec![], SparType::Bool, false,
            |_context, args| match args.first() { Some(Value::Int(value)) => Ok(Value::Bool(value % 2 == 0)), _ => Err(error("int.isEven receiver is not int")) },
        ),
        NativeMethod::sync(
            "int", "isOdd", SparType::Int, vec![], SparType::Bool, false,
            |_context, args| match args.first() { Some(Value::Int(value)) => Ok(Value::Bool(value % 2 != 0)), _ => Err(error("int.isOdd receiver is not int")) },
        ),
        NativeMethod::sync(
            "float", "abs", SparType::Float, vec![], SparType::Float, false,
            |_context, args| match args.first() { Some(Value::Float(value)) => Ok(Value::Float(value.abs())), _ => Err(error("float.abs receiver is not float")) },
        ),
        NativeMethod::sync(
            "float", "min", SparType::Float, vec![("other", SparType::Float)], SparType::Float, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Float(a)), Some(Value::Float(b))) => Ok(Value::Float(a.min(*b))), _ => Err(error("float.min expects float values")) },
        ),
        NativeMethod::sync(
            "float", "max", SparType::Float, vec![("other", SparType::Float)], SparType::Float, false,
            |_context, args| match (args.first(), args.get(1)) { (Some(Value::Float(a)), Some(Value::Float(b))) => Ok(Value::Float(a.max(*b))), _ => Err(error("float.max expects float values")) },
        ),
        NativeMethod::sync(
            "float", "clamp", SparType::Float, vec![("min", SparType::Float), ("max", SparType::Float)], SparType::Float, false,
            |_context, args| match (args.first(), args.get(1), args.get(2)) {
                (Some(Value::Float(value)), Some(Value::Float(min)), Some(Value::Float(max))) if min <= max => Ok(Value::Float(value.clamp(*min, *max))),
                (Some(Value::Float(_)), Some(Value::Float(_)), Some(Value::Float(_))) => Err(error("float.clamp min cannot exceed max")),
                _ => Err(error("float.clamp expects float values")),
            },
        ),
        NativeMethod::sync(
            "float", "floor", SparType::Float, vec![], SparType::Int, false,
            |_context, args| match args.first() { Some(Value::Float(value)) => Ok(Value::Int(value.floor() as i64)), _ => Err(error("float.floor receiver is not float")) },
        ),
        NativeMethod::sync(
            "float", "ceil", SparType::Float, vec![], SparType::Int, false,
            |_context, args| match args.first() { Some(Value::Float(value)) => Ok(Value::Int(value.ceil() as i64)), _ => Err(error("float.ceil receiver is not float")) },
        ),
        NativeMethod::sync(
            "float", "round", SparType::Float, vec![], SparType::Int, false,
            |_context, args| match args.first() { Some(Value::Float(value)) => Ok(Value::Int(value.round() as i64)), _ => Err(error("float.round receiver is not float")) },
        ),
        NativeMethod::sync(
            "float", "truncate", SparType::Float, vec![], SparType::Int, false,
            |_context, args| match args.first() { Some(Value::Float(value)) => Ok(Value::Int(value.trunc() as i64)), _ => Err(error("float.truncate receiver is not float")) },
        ),
        NativeMethod::sync(
            "float", "toString", SparType::Float, vec![], SparType::Str, false,
            |_context, args| match args.first() { Some(Value::Float(value)) => Ok(Value::String(value.to_string())), _ => Err(error("float.toString receiver is not float")) },
        ),
        NativeMethod::sync(
            "bool", "toString", SparType::Bool, vec![], SparType::Str, false,
            |_context, args| match args.first() { Some(Value::Bool(value)) => Ok(Value::String(value.to_string())), _ => Err(error("bool.toString receiver is not bool")) },
        ),
    ] {
        let owner = method.owner.clone();
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("{owner}.{name} registration must be unique"));
    }
}

fn register_option_methods(registry: &mut NativeRegistry) {
    let t = type_parameter("T");
    let u = type_parameter("U");
    let e = type_parameter("E");
    let option_t = applied("Option", vec![t.clone()]);
    let option_u = applied("Option", vec![u.clone()]);
    let result_te = applied("Result", vec![t.clone(), e.clone()]);
    let methods = [
        NativeMethod::sync(
            "Option", "isSome", option_t.clone(), vec![], SparType::Bool, false,
            |_context, args| Ok(Value::Bool(option_receiver(args)?.is_some())),
        ),
        NativeMethod::sync(
            "Option", "isNone", option_t.clone(), vec![], SparType::Bool, false,
            |_context, args| Ok(Value::Bool(option_receiver(args)?.is_none())),
        ),
        NativeMethod::sync(
            "Option", "unwrap", option_t.clone(), vec![], t.clone(), false,
            |_context, args| option_receiver(args)?.as_deref().cloned().ok_or_else(|| error("cannot unwrap None")),
        ),
        NativeMethod::sync(
            "Option", "expect", option_t.clone(), vec![("message", SparType::Str)], t.clone(), false,
            |_context, args| {
                let message = match args.get(1) {
                    Some(Value::String(value)) => value.as_str(),
                    _ => return Err(error("Option.expect message must be a str")),
                };
                option_receiver(args)?.as_deref().cloned().ok_or_else(|| error(message))
            },
        ),
        NativeMethod::sync(
            "Option", "unwrapOr", option_t.clone(), vec![("fallback", t.clone())], t.clone(), false,
            |_context, args| {
                let fallback = args.get(1).cloned().ok_or_else(|| error("missing Option.unwrapOr fallback"))?;
                Ok(option_receiver(args)?.as_deref().cloned().unwrap_or(fallback))
            },
        ),
        NativeMethod::intrinsic(
            "Option", "unwrapOrElse", option_t.clone(), vec![("fallback", callable(vec![], t.clone()))], t.clone(), false,
            crate::runtime::NativeIntrinsic::CoreOptionUnwrapOrElse,
        ),
        NativeMethod::intrinsic(
            "Option", "map", option_t.clone(), vec![("transform", callable(vec![("value", t.clone())], u.clone()))], option_u.clone(), false,
            crate::runtime::NativeIntrinsic::CoreOptionMap,
        ),
        NativeMethod::intrinsic(
            "Option", "filter", option_t.clone(), vec![("predicate", callable(vec![("value", t.clone())], SparType::Bool))], option_t.clone(), false,
            crate::runtime::NativeIntrinsic::CoreOptionFilter,
        ),
        NativeMethod::intrinsic(
            "Option", "andThen", option_t.clone(), vec![("transform", callable(vec![("value", t.clone())], option_u.clone()))], option_u.clone(), false,
            crate::runtime::NativeIntrinsic::CoreOptionAndThen,
        ),
        NativeMethod::sync(
            "Option", "or", option_t.clone(), vec![("other", option_t.clone())], option_t.clone(), false,
            |_context, args| {
                if option_receiver(args)?.is_some() {
                    return Ok(args[0].clone());
                }
                match args.get(1) {
                    Some(value @ Value::Option(_)) => Ok(value.clone()),
                    Some(other) => Err(error(format!("Option.or expected Option, found {}", other.type_name()))),
                    None => Err(error("missing Option.or other")),
                }
            },
        ),
        NativeMethod::intrinsic(
            "Option", "orElse", option_t.clone(), vec![("fallback", callable(vec![], option_t.clone()))], option_t.clone(), false,
            crate::runtime::NativeIntrinsic::CoreOptionOrElse,
        ),
        NativeMethod::sync(
            "Option", "okOr", option_t.clone(), vec![("error", e.clone())], result_te, false,
            |_context, args| {
                match option_receiver(args)? {
                    Some(value) => Ok(Value::Result(Ok(Box::new(value.as_ref().clone())))),
                    None => Ok(Value::Result(Err(crate::runtime::value::ErrBox::new(args.get(1).cloned().ok_or_else(|| error("missing Option.okOr error"))?)))),
                }
            },
        ),
    ];

    for method in methods {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("Option.{name} registration must be unique"));
    }

    for method in [
        NativeMethod::sync_mut(
            "Option", "take", option_t.clone(), vec![], option_t.clone(), false,
            |_context, receiver, _args| {
                let Value::Option(value) = receiver else {
                    return Err(error("Option.take receiver is not Option"));
                };
                Ok(Value::Option(value.take()))
            },
        ),
        NativeMethod::sync_mut(
            "Option", "replace", option_t.clone(), vec![("value", t.clone())], option_t, false,
            |_context, receiver, args| {
                let replacement = args.first().cloned().ok_or_else(|| error("missing Option.replace value"))?;
                let Value::Option(value) = receiver else {
                    return Err(error("Option.replace receiver is not Option"));
                };
                Ok(Value::Option(value.replace(Box::new(replacement))))
            },
        ),
    ] {
        let name = method.name.clone();
        registry.register_method(method).unwrap_or_else(|_| panic!("Option.{name} registration must be unique"));
    }
}

fn register_result_methods(registry: &mut NativeRegistry) {
    let t = type_parameter("T");
    let e = type_parameter("E");
    let u = type_parameter("U");
    let f = type_parameter("F");
    let result_te = applied("Result", vec![t.clone(), e.clone()]);
    let result_ue = applied("Result", vec![u.clone(), e.clone()]);
    let result_tf = applied("Result", vec![t.clone(), f.clone()]);
    let methods = [
        NativeMethod::sync(
            "Result", "isOk", result_te.clone(), vec![], SparType::Bool, false,
            |_context, args| Ok(Value::Bool(result_receiver(args)?.is_ok())),
        ),
        NativeMethod::sync(
            "Result", "isErr", result_te.clone(), vec![], SparType::Bool, false,
            |_context, args| Ok(Value::Bool(result_receiver(args)?.is_err())),
        ),
        NativeMethod::sync(
            "Result", "unwrap", result_te.clone(), vec![], t.clone(), false,
            |_context, args| match result_receiver(args)? {
                Ok(value) => Ok(value.as_ref().clone()), Err(_) => Err(error("cannot unwrap Err")),
            },
        ),
        NativeMethod::sync(
            "Result", "unwrapErr", result_te.clone(), vec![], e.clone(), false,
            |_context, args| match result_receiver(args)? {
                Ok(_) => Err(error("cannot unwrapErr Ok")), Err(value) => Ok(value.as_ref().clone()),
            },
        ),
        NativeMethod::sync(
            "Result", "expect", result_te.clone(), vec![("message", SparType::Str)], t.clone(), false,
            |_context, args| match result_receiver(args)? {
                Ok(value) => Ok(value.as_ref().clone()),
                Err(_) => match args.get(1) { Some(Value::String(message)) => Err(error(message)), _ => Err(error("Result.expect message must be a str")) },
            },
        ),
        NativeMethod::sync(
            "Result", "expectErr", result_te.clone(), vec![("message", SparType::Str)], e.clone(), false,
            |_context, args| match result_receiver(args)? {
                Err(value) => Ok(value.as_ref().clone()),
                Ok(_) => match args.get(1) { Some(Value::String(message)) => Err(error(message)), _ => Err(error("Result.expectErr message must be a str")) },
            },
        ),
        NativeMethod::sync(
            "Result", "unwrapOr", result_te.clone(), vec![("fallback", t.clone())], t.clone(), false,
            |_context, args| {
                let fallback = args.get(1).cloned().ok_or_else(|| error("missing Result.unwrapOr fallback"))?;
                match result_receiver(args)? { Ok(value) => Ok(value.as_ref().clone()), Err(_) => Ok(fallback) }
            },
        ),
        NativeMethod::intrinsic(
            "Result", "map", result_te.clone(), vec![("transform", callable(vec![("value", t.clone())], u.clone()))], result_ue.clone(), false,
            crate::runtime::NativeIntrinsic::CoreResultMap,
        ),
        NativeMethod::intrinsic(
            "Result", "mapErr", result_te.clone(), vec![("transform", callable(vec![("error", e.clone())], f.clone()))], result_tf.clone(), false,
            crate::runtime::NativeIntrinsic::CoreResultMapErr,
        ),
        NativeMethod::intrinsic(
            "Result", "andThen", result_te.clone(), vec![("transform", callable(vec![("value", t.clone())], result_ue.clone()))], result_ue, false,
            crate::runtime::NativeIntrinsic::CoreResultAndThen,
        ),
        NativeMethod::intrinsic(
            "Result", "orElse", result_te.clone(), vec![("fallback", callable(vec![("error", e.clone())], result_tf.clone()))], result_tf, false,
            crate::runtime::NativeIntrinsic::CoreResultOrElse,
        ),
        NativeMethod::sync(
            "Result", "ok", result_te.clone(), vec![], applied("Option", vec![t.clone()]), false,
            |_context, args| match result_receiver(args)? {
                Ok(value) => Ok(Value::Option(Some(Box::new(value.as_ref().clone())))), Err(_) => Ok(Value::Option(None)),
            },
        ),
        NativeMethod::sync(
            "Result", "err", result_te, vec![], applied("Option", vec![e]), false,
            |_context, args| match result_receiver(args)? {
                Err(value) => Ok(Value::Option(Some(Box::new(value.as_ref().clone())))), Ok(_) => Ok(Value::Option(None)),
            },
        ),
    ];

    for method in methods {
        let name = method.name.clone();
        let mut shell_method = method.clone();
        shell_method.owner = "ShellResult".into();
        if let SparType::Applied { name, .. } = &mut shell_method.receiver {
            *name = "ShellResult".into();
        }
        registry.register_method(method).unwrap_or_else(|_| panic!("Result.{name} registration must be unique"));
        registry.register_method(shell_method).unwrap_or_else(|_| panic!("ShellResult.{name} registration must be unique"));
    }
}

fn register_table_methods(registry: &mut NativeRegistry) {
    let table_t = applied("Table", vec![type_parameter("T")]);

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "length",
            table_t.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| table_length(args),
        ))
        .expect("Table.length registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "isEmpty",
            table_t.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| table_is_empty(args),
        ))
        .expect("Table.isEmpty registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "rows",
            table_t.clone(),
            vec![],
            SparType::List(Box::new(type_parameter("T"))),
            false,
            |_context, args| table_rows(args),
        ))
        .expect("Table.rows registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "columns",
            table_t,
            vec![],
            SparType::List(Box::new(SparType::Str)),
            false,
            |_context, args| table_columns(args),
        ))
        .expect("Table.columns registration must be unique");
}

fn length_value(args: &[Value]) -> Result<Value, crate::SparError> {
    let value = args
        .first()
        .ok_or_else(|| error("missing method receiver"))?;
    let length = match value {
        Value::String(value) => value.chars().count(),
        Value::Bytes(value) => value.len(),
        Value::List(value) => value.len(),
        Value::Object(value) => value.len(),
        Value::Map(value) => value.len(),
        Value::Table(value) => value.len(),
        other => {
            return Err(error(format!(
                "length() does not support runtime value {}",
                other.type_name()
            )))
        }
    };
    i64::try_from(length)
        .map(Value::Int)
        .map_err(|_| error("value length exceeds Spar int range"))
}

fn empty_value(args: &[Value]) -> Result<Value, crate::SparError> {
    let value = args
        .first()
        .ok_or_else(|| error("missing method receiver"))?;
    let empty = match value {
        Value::String(value) => value.is_empty(),
        Value::Bytes(value) => value.is_empty(),
        Value::List(value) => value.is_empty(),
        Value::Object(value) => value.is_empty(),
        Value::Map(value) => value.is_empty(),
        Value::Table(value) => value.is_empty(),
        other => {
            return Err(error(format!(
                "isEmpty() does not support runtime value {}",
                other.type_name()
            )))
        }
    };
    Ok(Value::Bool(empty))
}

fn string_receiver(args: &[Value]) -> Result<&str, crate::SparError> {
    match args.first() {
        Some(Value::String(value)) => Ok(value),
        Some(other) => Err(error(format!("str method received runtime value {}", other.type_name()))),
        None => Err(error("missing str method receiver")),
    }
}

fn string_arg<'a>(
    args: &'a [Value],
    index: usize,
    operation: &str,
    name: &str,
) -> Result<&'a str, crate::SparError> {
    match args.get(index) {
        Some(Value::String(value)) => Ok(value),
        Some(other) => Err(error(format!("{operation} argument '{name}' must be str, found {}", other.type_name()))),
        None => Err(error(format!("missing {operation} argument '{name}'"))),
    }
}

fn substring_value(args: &[Value]) -> Result<Value, crate::SparError> {
    let source = string_receiver(args)?;
    let chars = source.chars().collect::<Vec<_>>();
    let start = non_negative_index(args.get(1), "str.substring")?;
    let end = option_index(args.get(2), chars.len(), "str.substring")?;
    if start > end || end > chars.len() {
        return Err(error("str.substring range is out of bounds"));
    }
    Ok(Value::String(chars[start..end].iter().collect()))
}

fn string_index_of(args: &[Value]) -> Result<Value, crate::SparError> {
    let source = string_receiver(args)?;
    let needle = string_arg(args, 1, "str.indexOf", "needle")?;
    let Some(byte_index) = source.find(needle) else {
        return Ok(Value::Option(None));
    };
    let char_index = source[..byte_index].chars().count();
    let index = i64::try_from(char_index).map_err(|_| error("str.indexOf result exceeds Spar int range"))?;
    Ok(Value::Option(Some(Box::new(Value::Int(index)))))
}

fn list_receiver(args: &[Value]) -> Result<&[Value], crate::SparError> {
    match args.first() {
        Some(Value::List(values)) => Ok(values),
        Some(other) => Err(error(format!("List method received runtime value {}", other.type_name()))),
        None => Err(error("missing List method receiver")),
    }
}

fn non_negative_index(value: Option<&Value>, operation: &str) -> Result<usize, crate::SparError> {
    let Some(Value::Int(index)) = value else {
        return Err(error(format!("{operation} index must be an int")));
    };
    usize::try_from(*index).map_err(|_| error(format!("{operation} index cannot be negative")))
}

fn option_index(
    value: Option<&Value>,
    fallback: usize,
    operation: &str,
) -> Result<usize, crate::SparError> {
    match value {
        Some(Value::Option(None)) => Ok(fallback),
        Some(Value::Option(Some(value))) => match value.as_ref() {
            Value::Int(index) => usize::try_from(*index)
                .map_err(|_| error(format!("{operation} end cannot be negative"))),
            _ => Err(error(format!("{operation} end must be Option<int>"))),
        },
        _ => Err(error(format!("{operation} end must be Option<int>"))),
    }
}

fn map_receiver(args: &[Value]) -> Result<&crate::runtime::value::MapValue, crate::SparError> {
    match args.first() {
        Some(Value::Map(entries)) => Ok(entries),
        Some(other) => Err(error(format!(
            "Map method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing Map method receiver")),
    }
}

fn map_keys(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(map_receiver(args)?.keys().collect()))
}

fn map_values(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(map_receiver(args)?.values().cloned().collect()))
}

fn map_contains_key(args: &[Value]) -> Result<Value, crate::SparError> {
    let key = args
        .get(1)
        .ok_or_else(|| error("missing Map.containsKey key"))?;
    Ok(Value::Bool(map_receiver(args)?.contains_key(key)))
}

fn option_receiver(args: &[Value]) -> Result<&std::option::Option<Box<Value>>, crate::SparError> {
    match args.first() {
        Some(Value::Option(value)) => Ok(value),
        Some(other) => Err(error(format!(
            "Option method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing Option method receiver")),
    }
}

fn result_receiver(
    args: &[Value],
) -> Result<&std::result::Result<Box<Value>, crate::runtime::value::ErrBox>, crate::SparError> {
    match args.first() {
        Some(Value::Result(value)) => Ok(value),
        Some(other) => Err(error(format!(
            "Result method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing Result method receiver")),
    }
}

fn table_receiver(args: &[Value]) -> Result<&crate::runtime::TableValue, crate::SparError> {
    match args.first() {
        Some(Value::Table(table)) => Ok(table),
        Some(other) => Err(error(format!(
            "Table method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing table method receiver")),
    }
}

fn table_length(args: &[Value]) -> Result<Value, crate::SparError> {
    i64::try_from(table_receiver(args)?.len())
        .map(Value::Int)
        .map_err(|_| error("table length exceeds Spar int range"))
}

fn table_is_empty(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(table_receiver(args)?.is_empty()))
}

fn table_rows(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(Shared::from(table_receiver(args)?.rows().to_vec())))
}

fn table_columns(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(
        table_receiver(args)?
            .schema()
            .columns()
            .map(|column| Value::String(column.to_string()))
            .collect(),
    ))
}

/// `Record` is the dynamic bridge type: a field read from a Record is itself a
/// Record-typed dynamic value, and these methods turn dynamic values into
/// statically typed ones. Every conversion is checked at runtime.
fn register_record_methods(registry: &mut NativeRegistry) {
    let record = SparType::Named("Record".into());

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "has",
            record.clone(),
            vec![("name", SparType::Str)],
            SparType::Bool,
            false,
            |_context, args| {
                let name = super::support::string_arg(args, 1, "name")?;
                match args.first() {
                    Some(Value::Object(fields)) => Ok(Value::Bool(fields.contains_key(name))),
                    Some(other) => Err(error(format!(
                        "Record.has expected an object, found {}",
                        other.type_name()
                    ))),
                    None => Err(error("Record.has is missing its receiver")),
                }
            },
        ))
        .expect("Record.has registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "keys",
            record.clone(),
            vec![],
            SparType::List(Box::new(SparType::Str)),
            false,
            |_context, args| match args.first() {
                Some(Value::Object(fields)) => {
                    let mut keys = fields.keys().map(|key| key.to_string()).collect::<Vec<_>>();
                    keys.sort();
                    Ok(Value::List(keys.into_iter().map(Value::String).collect()))
                }
                Some(other) => Err(error(format!(
                    "Record.keys expected an object, found {}",
                    other.type_name()
                ))),
                None => Err(error("Record.keys is missing its receiver")),
            },
        ))
        .expect("Record.keys registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asStr",
            record.clone(),
            vec![],
            SparType::Str,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::String(_)) => Ok(value.clone()),
                other => Err(dynamic_mismatch("asStr", "str", other)),
            },
        ))
        .expect("Record.asStr registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asInt",
            record.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::Int(_)) => Ok(value.clone()),
                other => Err(dynamic_mismatch("asInt", "int", other)),
            },
        ))
        .expect("Record.asInt registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asFloat",
            record.clone(),
            vec![],
            SparType::Float,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::Float(_)) => Ok(value.clone()),
                // An integer widens to a float; the reverse never happens.
                Some(Value::Int(value)) => Ok(Value::Float(*value as f64)),
                other => Err(dynamic_mismatch("asFloat", "float", other)),
            },
        ))
        .expect("Record.asFloat registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asBool",
            record.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::Bool(_)) => Ok(value.clone()),
                other => Err(dynamic_mismatch("asBool", "bool", other)),
            },
        ))
        .expect("Record.asBool registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asList",
            record,
            vec![],
            SparType::List(Box::new(SparType::Named("Record".into()))),
            false,
            |_context, args| match args.first() {
                Some(value @ Value::List(_)) => Ok(value.clone()),
                other => Err(dynamic_mismatch("asList", "list", other)),
            },
        ))
        .expect("Record.asList registration must be unique");
}

fn dynamic_mismatch(method: &str, expected: &str, found: Option<&Value>) -> crate::SparError {
    error(format!(
        "{method}() expected a {expected} value, found {}",
        found.map_or("nothing", Value::type_name)
    ))
}
