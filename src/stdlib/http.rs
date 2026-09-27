use std::time::Duration;

use crate::ast::SparType;
use crate::error::SparError;
use crate::runtime::{NativeFunction, NativeMethod, NativeRegistry, Value};

use super::support::{error, int_arg, object, string_arg, value_to_serde};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeHttp",
            "request",
            vec![
                ("method", SparType::Str),
                ("url", SparType::Str),
                ("defaultHeaders", map_str_str_type()),
                ("headers", map_str_str_type()),
                ("query", map_str_str_type()),
                ("body", SparType::Str),
                ("jsonBody", SparType::Any),
                ("timeoutMillis", SparType::Int),
            ],
            SparType::Named("HttpResponse".into()),
            true,
            |_context, args| execute_request(args),
        ))
        .expect("nativeHttp::request registration must be unique");

    register_response_methods(registry);
}

fn execute_request(args: &[Value]) -> Result<Value, SparError> {
    let method = string_arg(args, 0, "method")?.to_ascii_uppercase();
    let url = string_arg(args, 1, "url")?;
    let default_headers = string_map_arg(args, 2, "defaultHeaders")?;
    let headers = string_map_arg(args, 3, "headers")?;
    let query = string_map_arg(args, 4, "query")?;
    let text_body = string_arg(args, 5, "body")?;
    let json_body = args
        .get(6)
        .ok_or_else(|| error("missing native argument 'jsonBody'"))?;
    let timeout_millis = int_arg(args, 7, "timeoutMillis")?;
    if timeout_millis < 0 {
        return Err(error("HTTP timeout cannot be negative"));
    }

    let has_json_body = !matches!(json_body, Value::Option(None));
    if has_json_body && !text_body.is_empty() {
        return Err(error("HTTP request cannot use both 'body' and 'jsonBody'"));
    }
    let body = if has_json_body {
        serde_json::to_string(&value_to_serde(json_body)?)
            .map_err(|e| error(format!("HTTP JSON body encoding failed: {e}")))?
    } else {
        text_body.to_string()
    };

    let mut request_headers = default_headers;
    request_headers.extend(headers);
    if has_json_body
        && !request_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        request_headers.push(("Content-Type".into(), "application/json".into()));
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_millis as u64))
        .build();
    let mut request = agent.request(&method, url);

    for (name, value) in request_headers {
        request = request.set(&name, &value);
    }
    for (name, value) in query {
        request = request.query(&name, &value);
    }

    let response = if body.is_empty() && matches!(method.as_str(), "GET" | "HEAD" | "DELETE") {
        request.call()
    } else {
        request.send_string(&body)
    };

    let (status, response) = match response {
        Ok(response) => (i64::from(response.status()), response),
        Err(ureq::Error::Status(code, response)) => (i64::from(code), response),
        Err(ureq::Error::Transport(transport)) => {
            return Err(error(format!("HTTP transport failed: {transport}")))
        }
    };

    let header_names = response.headers_names();
    let mut response_headers = Vec::with_capacity(header_names.len());
    for name in header_names {
        if let Some(value) = response.header(&name) {
            response_headers.push((Value::String(name), Value::String(value.to_string())));
        }
    }
    let content_type = response
        .header("Content-Type")
        .map(|value| Box::new(Value::String(value.to_string())));
    let body = response
        .into_string()
        .map_err(|e| error(format!("HTTP body read failed: {e}")))?;

    Ok(object([
        ("status", Value::Int(status)),
        ("body", Value::String(body)),
        ("headers", Value::Map(response_headers.into())),
        ("contentType", Value::Option(content_type)),
    ]))
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

fn map_str_str_type() -> SparType {
    SparType::Applied {
        name: "Map".into(),
        arguments: vec![SparType::Str, SparType::Str],
    }
}

fn string_map_arg(
    args: &[Value],
    index: usize,
    name: &str,
) -> Result<Vec<(String, String)>, SparError> {
    let value = args
        .get(index)
        .ok_or_else(|| error(format!("missing native argument '{name}'")))?;
    match value {
        Value::Map(entries) => entries
            .iter()
            .map(|(key, value)| match (key, value) {
                (Value::String(key), Value::String(value)) => Ok((key.clone(), value.clone())),
                (key, value) => Err(error(format!(
                    "native argument '{name}' expected Map<str, str>, received entry {} -> {}",
                    key.type_name(),
                    value.type_name()
                ))),
            })
            .collect(),
        Value::Object(entries) => entries
            .iter()
            .map(|(key, value)| match value {
                Value::String(value) => Ok((key.clone(), value.clone())),
                value => Err(error(format!(
                    "native argument '{name}' expected Map<str, str>, received value {}",
                    value.type_name()
                ))),
            })
            .collect(),
        other => Err(error(format!(
            "native argument '{name}' expected Map<str, str>, received {}",
            other.type_name()
        ))),
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
