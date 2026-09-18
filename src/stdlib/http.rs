use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, object, string_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry.register(NativeFunction::sync(
        "nativeHttp", "request",
        vec![("method", SparType::Str), ("url", SparType::Str), ("body", SparType::Str)],
        SparType::Named("HttpResponse".into()), true,
        |_context, args| {
            let method = string_arg(args, 0, "method")?.to_ascii_uppercase();
            let url = string_arg(args, 1, "url")?;
            let body = string_arg(args, 2, "body")?;
            let request = ureq::request(&method, url);
            let response = if body.is_empty() && matches!(method.as_str(), "GET" | "HEAD" | "DELETE") {
                request.call()
            } else {
                request.send_string(body)
            };
            let (status, response) = match response {
                Ok(response) => (i64::from(response.status()), response),
                Err(ureq::Error::Status(code, response)) => (i64::from(code), response),
                Err(ureq::Error::Transport(transport)) => {
                    return Err(error(format!("HTTP transport failed: {transport}")))
                }
            };
            let body = response.into_string().map_err(|e| error(format!("HTTP body read failed: {e}")))?;
            Ok(object([
                ("status", Value::Int(status)),
                ("body", Value::String(body)),
            ]))
        },
    )).expect("nativeHttp::request registration must be unique");
}
