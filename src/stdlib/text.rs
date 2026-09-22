use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeMethod, NativeRegistry, Value};

use super::support::{string_arg, string_list_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    register_unary(registry, "trim", trim_impl);
    register_unary(registry, "lower", lower_impl);
    register_unary(registry, "upper", upper_impl);

    register_predicate(registry, "contains", contains_impl);
    register_predicate(registry, "startsWith", starts_with_impl);
    register_predicate(registry, "endsWith", ends_with_impl);

    registry
        .register(NativeFunction::sync(
            "nativeText",
            "parseSize",
            vec![("value", SparType::Str)],
            SparType::Int,
            true,
            parse_size_impl,
        ))
        .expect("nativeText::parseSize registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "parseSize",
            SparType::Str,
            vec![],
            SparType::Int,
            false,
            parse_size_impl,
        ))
        .expect("str.parseSize registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeText",
            "replace",
            vec![
                ("value", SparType::Str),
                ("from", SparType::Str),
                ("to", SparType::Str),
            ],
            SparType::Str,
            true,
            replace_impl,
        ))
        .expect("nativeText::replace registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "replace",
            SparType::Str,
            vec![("from", SparType::Str), ("to", SparType::Str)],
            SparType::Str,
            false,
            replace_impl,
        ))
        .expect("str.replace registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeText",
            "split",
            vec![("value", SparType::Str), ("separator", SparType::Str)],
            SparType::List(Box::new(SparType::Str)),
            true,
            split_impl,
        ))
        .expect("nativeText::split registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "split",
            SparType::Str,
            vec![("separator", SparType::Str)],
            SparType::List(Box::new(SparType::Str)),
            false,
            split_impl,
        ))
        .expect("str.split registration must be unique");

    let string_list = SparType::List(Box::new(SparType::Str));
    registry
        .register(NativeFunction::sync(
            "nativeText",
            "join",
            vec![
                ("values", string_list.clone()),
                ("separator", SparType::Str),
            ],
            SparType::Str,
            true,
            join_impl,
        ))
        .expect("nativeText::join registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "List",
            "join",
            string_list,
            vec![("separator", SparType::Str)],
            SparType::Str,
            false,
            join_impl,
        ))
        .expect("List<str>.join registration must be unique");
}

fn register_unary(
    registry: &mut NativeRegistry,
    name: &str,
    callback: fn(&mut crate::runtime::RuntimeContext, &[Value]) -> Result<Value, crate::SparError>,
) {
    registry
        .register(NativeFunction::sync(
            "nativeText",
            name,
            vec![("value", SparType::Str)],
            SparType::Str,
            true,
            callback,
        ))
        .unwrap_or_else(|_| panic!("nativeText::{name} registration must be unique"));
    registry
        .register_method(NativeMethod::sync(
            "str",
            name,
            SparType::Str,
            vec![],
            SparType::Str,
            false,
            callback,
        ))
        .unwrap_or_else(|_| panic!("str.{name} registration must be unique"));
}

fn register_predicate(
    registry: &mut NativeRegistry,
    name: &str,
    callback: fn(&mut crate::runtime::RuntimeContext, &[Value]) -> Result<Value, crate::SparError>,
) {
    registry
        .register(NativeFunction::sync(
            "nativeText",
            name,
            vec![("value", SparType::Str), ("needle", SparType::Str)],
            SparType::Bool,
            true,
            callback,
        ))
        .unwrap_or_else(|_| panic!("nativeText::{name} registration must be unique"));
    registry
        .register_method(NativeMethod::sync(
            "str",
            name,
            SparType::Str,
            vec![("needle", SparType::Str)],
            SparType::Bool,
            false,
            callback,
        ))
        .unwrap_or_else(|_| panic!("str.{name} registration must be unique"));
}

fn trim_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(
        string_arg(args, 0, "value")?.trim().to_string(),
    ))
}

fn lower_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(string_arg(args, 0, "value")?.to_lowercase()))
}

fn upper_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(string_arg(args, 0, "value")?.to_uppercase()))
}

fn contains_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(
        string_arg(args, 0, "value")?.contains(string_arg(args, 1, "needle")?),
    ))
}

fn starts_with_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(
        string_arg(args, 0, "value")?.starts_with(string_arg(args, 1, "needle")?),
    ))
}

fn ends_with_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(
        string_arg(args, 0, "value")?.ends_with(string_arg(args, 1, "needle")?),
    ))
}

fn parse_size_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    let text = string_arg(args, 0, "value")?;
    parse_size(text)
        .map(Value::Int)
        .ok_or_else(|| super::support::error(format!("could not parse size '{text}'")))
}

/// Parses a human-readable byte size: a bare number (bytes), or a number
/// followed by a unit -- "10GB", "1.5 GiB", "512K", "2 terabytes". Case and
/// internal spacing/commas are ignored, and a trailing "s" is dropped so
/// "gigabytes" and "GB" mean the same thing.
///
/// An `i` before the final `b` (`KiB`, `MiB`, ...) is binary, base 1024;
/// otherwise the unit is decimal, base 1000 -- matching what a directory
/// listing's own `size` column displays (`ls`'s "10.5 GB" means
/// `parseSize("10.5GB")`, not `parseSize("10.5GiB")`).
fn parse_size(text: &str) -> Option<i64> {
    let cleaned = text.trim().replace(',', "");
    let digits_end = cleaned
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit() || *c == '.' || *c == '-')
        .last()?
        .0
        + 1;
    let number: f64 = cleaned[..digits_end].parse().ok()?;
    let mut unit = cleaned[digits_end..].trim().to_ascii_lowercase();
    if unit.ends_with('s') {
        unit.pop();
    }
    if unit.is_empty() || unit == "b" || unit == "byte" {
        return Some(number as i64);
    }

    const UNITS: [(char, &str, &str); 6] = [
        ('k', "kilobyte", "kibibyte"),
        ('m', "megabyte", "mebibyte"),
        ('g', "gigabyte", "gibibyte"),
        ('t', "terabyte", "tebibyte"),
        ('p', "petabyte", "pebibyte"),
        ('e', "exabyte", "exbibyte"),
    ];
    for (index, (prefix, decimal_name, binary_name)) in UNITS.iter().enumerate() {
        let exponent = i32::try_from(index).ok()? + 1;
        if unit == format!("{prefix}ib") || unit == *binary_name {
            return Some((number * 1024f64.powi(exponent)) as i64);
        }
        if unit == format!("{prefix}b") || unit == *decimal_name || unit == prefix.to_string() {
            return Some((number * 1000f64.powi(exponent)) as i64);
        }
    }
    None
}

fn replace_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(string_arg(args, 0, "value")?.replace(
        string_arg(args, 1, "from")?,
        string_arg(args, 2, "to")?,
    )))
}

fn split_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    let value = string_arg(args, 0, "value")?;
    let separator = string_arg(args, 1, "separator")?;
    Ok(Value::List(
        value
            .split(separator)
            .map(|part| Value::String(part.to_string()))
            .collect(),
    ))
}

fn join_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(
        string_list_arg(args, 0, "values")?.join(string_arg(args, 1, "separator")?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_numbers_and_bytes_pass_through() {
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("512 B"), Some(512));
        assert_eq!(parse_size("3 bytes"), Some(3));
    }

    #[test]
    fn decimal_units_use_powers_of_1000() {
        assert_eq!(parse_size("1KB"), Some(1_000));
        assert_eq!(parse_size("10GB"), Some(10_000_000_000));
        assert_eq!(parse_size("1.5 gigabytes"), Some(1_500_000_000));
        assert_eq!(parse_size("2M"), Some(2_000_000));
    }

    #[test]
    fn an_i_before_the_final_b_means_binary_powers_of_1024() {
        assert_eq!(parse_size("1KiB"), Some(1_024));
        assert_eq!(parse_size("1.5GiB"), Some((1.5 * 1024f64.powi(3)) as i64));
        assert_eq!(parse_size("2 kibibytes"), Some(2_048));
    }

    #[test]
    fn matches_what_a_listings_size_column_displays() {
        // The `ls` table's own "10.5 GB" is decimal, so it round-trips
        // through the same string this function parses.
        assert_eq!(parse_size("30.3GB"), Some(30_300_000_000));
    }

    #[test]
    fn case_spacing_commas_and_plurals_do_not_matter() {
        assert_eq!(parse_size("  10 Gb  "), Some(10_000_000_000));
        assert_eq!(parse_size("1,024KB"), Some(1_024_000));
    }

    #[test]
    fn unparseable_or_empty_input_is_none() {
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("GB"), None);
        assert_eq!(parse_size("10 furlongs"), None);
    }
}
