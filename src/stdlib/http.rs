use crate::ast::SparType;
use crate::error::SparError;
use crate::runtime::{NativeFunction, NativeMethod, NativeRegistry, Value};
use crate::structured_codec::StructuredFormatRegistry;

use super::support::{error, object, string_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeHttp",
            "request",
            vec![
                ("method", SparType::Str),
                ("url", SparType::Str),
                ("body", SparType::Str),
            ],
            SparType::Named("HttpResponse".into()),
            true,
            |_context, args| {
                let method = string_arg(args, 0, "method")?.to_ascii_uppercase();
                let url = string_arg(args, 1, "url")?;
                let body = string_arg(args, 2, "body")?;
                let request = ureq::request(&method, url);
                let response =
                    if body.is_empty() && matches!(method.as_str(), "GET" | "HEAD" | "DELETE") {
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
                // The Content-Type header (empty when absent) tells a caller
                // whether the body is JSON, HTML, XML, plain text, ...
                let content_type = response.header("Content-Type").unwrap_or("").to_string();
                let body = response
                    .into_string()
                    .map_err(|e| error(format!("HTTP body read failed: {e}")))?;
                Ok(object([
                    ("status", Value::Int(status)),
                    ("body", Value::String(body)),
                    ("contentType", Value::String(content_type)),
                ]))
            },
        ))
        .expect("nativeHttp::request registration must be unique");

    register_response_methods(registry);
}

fn register_response_methods(registry: &mut NativeRegistry) {
    let response = SparType::Named("HttpResponse".into());

    registry
        .register_method(NativeMethod::sync(
            "HttpResponse",
            "text",
            response.clone(),
            vec![],
            SparType::Str,
            false,
            |_context, args| {
                Ok(Value::String(
                    response_body(args, "HttpResponse.text")?.to_string(),
                ))
            },
        ))
        .expect("HttpResponse.text registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "HttpResponse",
            "json",
            response.clone(),
            vec![],
            SparType::Named("Record".into()),
            false,
            |_context, args| {
                let body = response_body(args, "HttpResponse.json")?;
                let mut values =
                    StructuredFormatRegistry::builtin().decode_bytes("json", body.as_bytes())?;
                if values.len() != 1 {
                    return Err(error(
                        "HttpResponse.json() expected exactly one JSON document",
                    ));
                }
                match values.pop().expect("single decoded JSON value") {
                    value @ Value::Object(_) => Ok(value),
                    _ => Err(error(
                        "HttpResponse.json() expects a JSON object; use text() with std/json.parse for other JSON roots",
                    )),
                }
            },
        ))
        .expect("HttpResponse.json registration must be unique");

    for (name, predicate) in [
        ("isSuccess", status_is_success as fn(i64) -> bool),
        ("isClientError", status_is_client_error as fn(i64) -> bool),
        ("isServerError", status_is_server_error as fn(i64) -> bool),
    ] {
        registry
            .register_method(NativeMethod::sync(
                "HttpResponse",
                name,
                response.clone(),
                vec![],
                SparType::Bool,
                false,
                move |_context, args| Ok(Value::Bool(predicate(response_status(args, name)?))),
            ))
            .unwrap_or_else(|_| panic!("HttpResponse.{name} registration must be unique"));
    }
}

fn response_fields<'a>(
    args: &'a [Value],
    operation: &str,
) -> Result<&'a indexmap::IndexMap<String, Value>, SparError> {
    match args.first() {
        Some(Value::Object(fields)) => Ok(fields),
        Some(value) => Err(error(format!(
            "{operation} expected HttpResponse receiver, received {}",
            value.type_name()
        ))),
        None => Err(error(format!(
            "{operation} is missing its HttpResponse receiver"
        ))),
    }
}

fn response_body<'a>(args: &'a [Value], operation: &str) -> Result<&'a str, SparError> {
    let fields = response_fields(args, operation)?;
    match fields.get("body") {
        Some(Value::String(body)) => Ok(body),
        Some(value) => Err(error(format!(
            "{operation} expected HttpResponse.body to be str, received {}",
            value.type_name()
        ))),
        None => Err(error(format!(
            "{operation} received an HttpResponse without body"
        ))),
    }
}

fn response_status(args: &[Value], operation: &str) -> Result<i64, SparError> {
    let fields = response_fields(args, operation)?;
    match fields.get("status") {
        Some(Value::Int(status)) => Ok(*status),
        Some(value) => Err(error(format!(
            "{operation} expected HttpResponse.status to be int, received {}",
            value.type_name()
        ))),
        None => Err(error(format!(
            "{operation} received an HttpResponse without status"
        ))),
    }
}

fn status_is_success(status: i64) -> bool {
    (200..300).contains(&status)
}

fn status_is_client_error(status: i64) -> bool {
    (400..500).contains(&status)
}

fn status_is_server_error(status: i64) -> bool {
    (500..600).contains(&status)
}
