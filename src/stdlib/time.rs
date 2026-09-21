use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, int_arg, string_arg};

const MILLIS_PER_DAY: i64 = 86_400_000;

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeTime",
            "nowMillis",
            vec![],
            SparType::Int,
            true,
            |_context, _args| {
                let millis = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| error(format!("system clock is before Unix epoch: {e}")))?
                    .as_millis();
                Ok(Value::Int(i64::try_from(millis).unwrap_or(i64::MAX)))
            },
        ))
        .expect("nativeTime::nowMillis registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTime",
            "sleepMillis",
            vec![("millis", SparType::Int)],
            SparType::Void,
            true,
            |_context, args| {
                let millis = int_arg(args, 0, "millis")?;
                if millis < 0 {
                    return Err(error("sleep duration cannot be negative"));
                }
                std::thread::sleep(Duration::from_millis(millis as u64));
                Ok(Value::Void)
            },
        ))
        .expect("nativeTime::sleepMillis registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTime",
            "formatIso8601",
            vec![("millis", SparType::Int)],
            SparType::Str,
            true,
            |_context, args| {
                let millis = int_arg(args, 0, "millis")?;
                format_iso8601(millis).map(Value::String)
            },
        ))
        .expect("nativeTime::formatIso8601 registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTime",
            "parseIso8601",
            vec![("value", SparType::Str)],
            SparType::Int,
            true,
            |_context, args| parse_iso8601(string_arg(args, 0, "value")?).map(Value::Int),
        ))
        .expect("nativeTime::parseIso8601 registration must be unique");
}

fn format_iso8601(millis: i64) -> Result<String, crate::SparError> {
    let days = millis.div_euclid(MILLIS_PER_DAY);
    let day_millis = millis.rem_euclid(MILLIS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    if !(0..=9999).contains(&year) {
        return Err(error(
            "ISO-8601 formatting supports years 0000 through 9999",
        ));
    }
    let hour = day_millis / 3_600_000;
    let minute = (day_millis % 3_600_000) / 60_000;
    let second = (day_millis % 60_000) / 1_000;
    let milliseconds = day_millis % 1_000;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milliseconds:03}Z"
    ))
}

fn parse_iso8601(value: &str) -> Result<i64, crate::SparError> {
    // v1 deliberately accepts one deterministic UTC representation:
    // YYYY-MM-DDTHH:MM:SS.mmmZ. It is small, locale-free and round-trips
    // exactly with `formatIso8601`.
    let bytes = value.as_bytes();
    let shape_ok = bytes.len() == 24
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':')
        && bytes.get(19) == Some(&b'.')
        && bytes.get(23) == Some(&b'Z');
    if !shape_ok || !value.is_ascii() {
        return Err(invalid_iso(value));
    }

    let year = parse_digits(value, 0, 4)? as i64;
    let month = parse_digits(value, 5, 7)? as i64;
    let day = parse_digits(value, 8, 10)? as i64;
    let hour = parse_digits(value, 11, 13)? as i64;
    let minute = parse_digits(value, 14, 16)? as i64;
    let second = parse_digits(value, 17, 19)? as i64;
    let millis = parse_digits(value, 20, 23)? as i64;

    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(invalid_iso(value));
    }

    let days = days_from_civil(year, month, day);
    days.checked_mul(MILLIS_PER_DAY)
        .and_then(|base| base.checked_add(hour * 3_600_000))
        .and_then(|base| base.checked_add(minute * 60_000))
        .and_then(|base| base.checked_add(second * 1_000))
        .and_then(|base| base.checked_add(millis))
        .ok_or_else(|| invalid_iso(value))
}

fn parse_digits(value: &str, start: usize, end: usize) -> Result<u32, crate::SparError> {
    value[start..end]
        .parse::<u32>()
        .map_err(|_| invalid_iso(value))
}

fn invalid_iso(value: &str) -> crate::SparError {
    error(format!(
        "invalid ISO-8601 UTC timestamp '{value}'; expected YYYY-MM-DDTHH:MM:SS.mmmZ"
    ))
}

fn is_leap_year(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

// Howard Hinnant's civil calendar conversion, expressed with Euclidean
// division so negative Unix dates behave correctly too.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_round_trips_known_dates() {
        for (millis, formatted) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_827_696_789, "2000-02-29T12:34:56.789Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
        ] {
            assert_eq!(format_iso8601(millis).unwrap(), formatted);
            assert_eq!(parse_iso8601(formatted).unwrap(), millis);
        }
    }

    #[test]
    fn iso8601_rejects_invalid_calendar_dates() {
        assert!(parse_iso8601("2026-02-30T00:00:00.000Z").is_err());
        assert!(parse_iso8601("2026-13-01T00:00:00.000Z").is_err());
        assert!(parse_iso8601("not-a-time").is_err());
    }
}
