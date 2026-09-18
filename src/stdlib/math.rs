use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, float_arg, int_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry.register(NativeFunction::sync(
        "nativeMath", "absInt", vec![("value", SparType::Int)], SparType::Int, true,
        |_context, args| int_arg(args, 0, "value")?.checked_abs().map(Value::Int).ok_or_else(|| error("abs overflows Spar int")),
    )).expect("nativeMath::absInt registration must be unique");
    for (name, op) in [
        ("abs", f64::abs as fn(f64) -> f64),
        ("round", f64::round),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
        ("sqrt", f64::sqrt),
    ] {
        registry.register(NativeFunction::sync(
            "nativeMath", name, vec![("value", SparType::Float)], SparType::Float, true,
            move |_context, args| Ok(Value::Float(op(float_arg(args, 0, "value")?))),
        )).expect("nativeMath unary registration must be unique");
    }
    for (name, op) in [
        ("min", f64::min as fn(f64, f64) -> f64),
        ("max", f64::max as fn(f64, f64) -> f64),
        ("pow", f64::powf as fn(f64, f64) -> f64),
    ] {
        registry.register(NativeFunction::sync(
            "nativeMath", name,
            vec![("left", SparType::Float), ("right", SparType::Float)],
            SparType::Float, true,
            move |_context, args| Ok(Value::Float(op(float_arg(args, 0, "left")?, float_arg(args, 1, "right")?))),
        )).expect("nativeMath binary registration must be unique");
    }
    registry.register(NativeFunction::sync(
        "nativeMath", "minInt", vec![("left", SparType::Int), ("right", SparType::Int)], SparType::Int, true,
        |_context, args| Ok(Value::Int(int_arg(args, 0, "left")?.min(int_arg(args, 1, "right")?))),
    )).expect("nativeMath::minInt registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeMath", "maxInt", vec![("left", SparType::Int), ("right", SparType::Int)], SparType::Int, true,
        |_context, args| Ok(Value::Int(int_arg(args, 0, "left")?.max(int_arg(args, 1, "right")?))),
    )).expect("nativeMath::maxInt registration must be unique");
}
