use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, int_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeRandom",
            "bytes",
            vec![("count", SparType::Int)],
            SparType::Named("Bytes".into()),
            true,
            |_context, args| {
                let count = int_arg(args, 0, "count")?;
                if !(0..=16_777_216).contains(&count) {
                    return Err(error("random byte count must be between 0 and 16777216"));
                }
                let mut bytes = vec![0u8; count as usize];
                getrandom::getrandom(&mut bytes)
                    .map_err(|e| error(format!("OS random source failed: {e}")))?;
                Ok(Value::Bytes(bytes))
            },
        ))
        .expect("nativeRandom::bytes registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeRandom",
            "randomInt",
            vec![("min", SparType::Int), ("max", SparType::Int)],
            SparType::Int,
            true,
            |_context, args| {
                let min = int_arg(args, 0, "min")?;
                let max = int_arg(args, 1, "max")?;
                if max < min {
                    return Err(error("random int max must be greater than or equal to min"));
                }
                let span = (max as i128 - min as i128 + 1) as u128;
                let mut bytes = [0u8; 16];
                getrandom::getrandom(&mut bytes)
                    .map_err(|e| error(format!("OS random source failed: {e}")))?;
                let sample = u128::from_le_bytes(bytes) % span;
                Ok(Value::Int((min as i128 + sample as i128) as i64))
            },
        ))
        .expect("nativeRandom::randomInt registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeRandom",
            "randomFloat",
            vec![],
            SparType::Float,
            true,
            |_context, _args| {
                let mut bytes = [0u8; 8];
                getrandom::getrandom(&mut bytes)
                    .map_err(|e| error(format!("OS random source failed: {e}")))?;
                let sample = u64::from_le_bytes(bytes) >> 11;
                Ok(Value::Float((sample as f64) / ((1u64 << 53) as f64)))
            },
        ))
        .expect("nativeRandom::randomFloat registration must be unique");
}
