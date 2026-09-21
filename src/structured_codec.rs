use std::collections::{HashMap, HashSet};

use crate::error::{Span, SparError};
use crate::runtime::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StructuredFormat {
    Json,
    JsonLines,
    Csv,
    Tsv,
    Lines,
    Yaml,
    Toml,
    /// The whole input as one `str`; going out, `str` values are written verbatim.
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecMode {
    Streaming,
    Document,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecDescriptor {
    format: StructuredFormat,
    name: &'static str,
    aliases: &'static [&'static str],
    mode: CodecMode,
}

impl CodecDescriptor {
    pub fn format(&self) -> StructuredFormat {
        self.format
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    pub fn mode(&self) -> CodecMode {
        self.mode
    }
}

const JSON: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::Json,
    name: "json",
    aliases: &[],
    mode: CodecMode::Document,
};
const JSONL: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::JsonLines,
    name: "jsonl",
    aliases: &["ndjson"],
    mode: CodecMode::Streaming,
};
const CSV: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::Csv,
    name: "csv",
    aliases: &[],
    mode: CodecMode::Streaming,
};
const TSV: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::Tsv,
    name: "tsv",
    aliases: &[],
    mode: CodecMode::Streaming,
};
const LINES: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::Lines,
    name: "lines",
    aliases: &["line"],
    mode: CodecMode::Streaming,
};
const YAML: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::Yaml,
    name: "yaml",
    aliases: &["yml"],
    mode: CodecMode::Document,
};
const TOML: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::Toml,
    name: "toml",
    aliases: &[],
    mode: CodecMode::Document,
};

const TEXT: CodecDescriptor = CodecDescriptor {
    format: StructuredFormat::Text,
    name: "text",
    aliases: &["txt"],
    mode: CodecMode::Document,
};

const BUILTIN_DESCRIPTORS: &[CodecDescriptor] = &[JSON, JSONL, CSV, TSV, LINES, YAML, TOML, TEXT];

#[derive(Clone, Debug)]
pub struct StructuredFormatRegistry {
    by_name: HashMap<String, CodecDescriptor>,
}

impl Default for StructuredFormatRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl StructuredFormatRegistry {
    pub fn builtin() -> Self {
        let mut by_name = HashMap::new();
        for descriptor in BUILTIN_DESCRIPTORS {
            by_name.insert(descriptor.name.to_string(), *descriptor);
            for alias in descriptor.aliases {
                by_name.insert((*alias).to_string(), *descriptor);
            }
        }
        Self { by_name }
    }

    pub fn descriptor(&self, name: &str) -> Option<CodecDescriptor> {
        self.by_name.get(&normalize_name(name)).copied()
    }

    pub fn descriptors(&self) -> Vec<CodecDescriptor> {
        BUILTIN_DESCRIPTORS.to_vec()
    }

    pub fn parser(&self, name: &str) -> Result<StructuredParser, SparError> {
        let descriptor = self.descriptor(name).ok_or_else(|| unknown_format(name))?;
        Ok(StructuredParser::new(descriptor.format))
    }

    pub fn serializer(&self, name: &str) -> Result<StructuredSerializer, SparError> {
        let descriptor = self.descriptor(name).ok_or_else(|| unknown_format(name))?;
        Ok(StructuredSerializer::new(descriptor.format))
    }

    /// Explicit byte-to-value bridge for finite inputs. Streaming callers should
    /// keep the parser returned by `parser` and feed chunks as they arrive.
    pub fn decode_bytes(&self, name: &str, bytes: &[u8]) -> Result<Vec<Value>, SparError> {
        let mut parser = self.parser(name)?;
        let mut values = parser.push(bytes)?;
        values.extend(parser.finish()?);
        Ok(values)
    }

    /// Explicit value-to-byte bridge for finite inputs. Streaming callers should
    /// keep the serializer returned by `serializer` and write each emitted chunk.
    pub fn encode_values(&self, name: &str, values: &[Value]) -> Result<Vec<u8>, SparError> {
        let mut serializer = self.serializer(name)?;
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend(serializer.push(value)?);
        }
        bytes.extend(serializer.finish()?);
        Ok(bytes)
    }
}

#[derive(Debug)]
pub struct StructuredParser {
    format: StructuredFormat,
    state: ParserState,
    finished: bool,
}

#[derive(Debug)]
enum ParserState {
    Buffered(Vec<u8>),
    Lines(Vec<u8>),
    JsonLines(Vec<u8>),
    Delimited {
        delimiter: u8,
        pending: Vec<u8>,
        headers: Option<Vec<String>>,
    },
}

impl StructuredParser {
    fn new(format: StructuredFormat) -> Self {
        let state = match format {
            StructuredFormat::Json
            | StructuredFormat::Yaml
            | StructuredFormat::Toml
            | StructuredFormat::Text => ParserState::Buffered(Vec::new()),
            StructuredFormat::JsonLines => ParserState::JsonLines(Vec::new()),
            StructuredFormat::Lines => ParserState::Lines(Vec::new()),
            StructuredFormat::Csv => ParserState::Delimited {
                delimiter: b',',
                pending: Vec::new(),
                headers: None,
            },
            StructuredFormat::Tsv => ParserState::Delimited {
                delimiter: b'\t',
                pending: Vec::new(),
                headers: None,
            },
        };
        Self {
            format,
            state,
            finished: false,
        }
    }

    pub fn format(&self) -> StructuredFormat {
        self.format
    }

    pub fn mode(&self) -> CodecMode {
        match self.format {
            StructuredFormat::Json
            | StructuredFormat::Yaml
            | StructuredFormat::Toml
            | StructuredFormat::Text => CodecMode::Document,
            StructuredFormat::JsonLines
            | StructuredFormat::Csv
            | StructuredFormat::Tsv
            | StructuredFormat::Lines => CodecMode::Streaming,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, SparError> {
        if self.finished {
            return Err(codec_error("structured parser is already finished"));
        }
        match &mut self.state {
            ParserState::Buffered(buffer) => {
                buffer.extend_from_slice(bytes);
                Ok(Vec::new())
            }
            ParserState::Lines(pending) => {
                pending.extend_from_slice(bytes);
                drain_lines(pending, false)
            }
            ParserState::JsonLines(pending) => {
                pending.extend_from_slice(bytes);
                drain_json_lines(pending, false)
            }
            ParserState::Delimited {
                delimiter,
                pending,
                headers,
            } => {
                pending.extend_from_slice(bytes);
                drain_delimited(pending, *delimiter, headers, false)
            }
        }
    }

    pub fn finish(mut self) -> Result<Vec<Value>, SparError> {
        if self.finished {
            return Err(codec_error("structured parser is already finished"));
        }
        self.finished = true;
        match &mut self.state {
            ParserState::Buffered(buffer) => parse_document(self.format, buffer),
            ParserState::Lines(pending) => drain_lines(pending, true),
            ParserState::JsonLines(pending) => drain_json_lines(pending, true),
            ParserState::Delimited {
                delimiter,
                pending,
                headers,
            } => drain_delimited(pending, *delimiter, headers, true),
        }
    }
}

#[derive(Debug)]
pub struct StructuredSerializer {
    format: StructuredFormat,
    state: SerializerState,
    finished: bool,
}

#[derive(Debug)]
enum SerializerState {
    Buffered(Vec<Value>),
    JsonLines,
    Lines,
    Text,
    Delimited {
        delimiter: u8,
        headers: Option<Vec<String>>,
    },
}

impl StructuredSerializer {
    fn new(format: StructuredFormat) -> Self {
        let state = match format {
            StructuredFormat::Json | StructuredFormat::Yaml | StructuredFormat::Toml => {
                SerializerState::Buffered(Vec::new())
            }
            StructuredFormat::Text => SerializerState::Text,
            StructuredFormat::JsonLines => SerializerState::JsonLines,
            StructuredFormat::Lines => SerializerState::Lines,
            StructuredFormat::Csv => SerializerState::Delimited {
                delimiter: b',',
                headers: None,
            },
            StructuredFormat::Tsv => SerializerState::Delimited {
                delimiter: b'\t',
                headers: None,
            },
        };
        Self {
            format,
            state,
            finished: false,
        }
    }

    pub fn format(&self) -> StructuredFormat {
        self.format
    }

    pub fn mode(&self) -> CodecMode {
        match self.format {
            StructuredFormat::Json | StructuredFormat::Yaml | StructuredFormat::Toml => {
                CodecMode::Document
            }
            StructuredFormat::JsonLines
            | StructuredFormat::Csv
            | StructuredFormat::Tsv
            | StructuredFormat::Lines
            | StructuredFormat::Text => CodecMode::Streaming,
        }
    }

    pub fn push(&mut self, value: &Value) -> Result<Vec<u8>, SparError> {
        if self.finished {
            return Err(codec_error("structured serializer is already finished"));
        }

        match &mut self.state {
            SerializerState::Buffered(values) => {
                values.push(value.clone());
                Ok(Vec::new())
            }
            SerializerState::JsonLines => encode_json_lines(value),
            SerializerState::Lines => encode_lines(value),
            SerializerState::Text => encode_text(value),
            SerializerState::Delimited { delimiter, headers } => {
                encode_delimited(value, *delimiter, headers)
            }
        }
    }

    pub fn finish(mut self) -> Result<Vec<u8>, SparError> {
        if self.finished {
            return Err(codec_error("structured serializer is already finished"));
        }
        self.finished = true;
        match self.state {
            SerializerState::Buffered(values) => serialize_document(self.format, values),
            SerializerState::JsonLines
            | SerializerState::Lines
            | SerializerState::Text
            | SerializerState::Delimited { .. } => Ok(Vec::new()),
        }
    }
}

fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn unknown_format(name: &str) -> SparError {
    codec_error(format!("unknown structured format '{}'", name.trim()))
}

fn codec_error(message: impl Into<String>) -> SparError {
    SparError::EvalError {
        message: message.into(),
        span: Span::dummy(),
    }
}

fn parse_document(format: StructuredFormat, bytes: &[u8]) -> Result<Vec<Value>, SparError> {
    if format == StructuredFormat::Text {
        // The whole input as a single string; no input means no values.
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|error| codec_error(format!("text input is not valid UTF-8: {error}")))?;
        return Ok(vec![Value::String(text.to_string())]);
    }
    if bytes.is_empty() {
        return Err(codec_error(format!(
            "{} document is empty",
            format_name(format)
        )));
    }

    let json = match format {
        StructuredFormat::Json => serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|error| codec_error(format!("invalid JSON: {error}")))?,
        StructuredFormat::Yaml => serde_yaml::from_slice::<serde_json::Value>(bytes)
            .map_err(|error| codec_error(format!("invalid YAML: {error}")))?,
        StructuredFormat::Toml => {
            let text = std::str::from_utf8(bytes)
                .map_err(|error| codec_error(format!("TOML input is not valid UTF-8: {error}")))?;
            let value = toml::from_str::<toml::Value>(text)
                .map_err(|error| codec_error(format!("invalid TOML: {error}")))?;
            serde_json::to_value(value)
                .map_err(|error| codec_error(format!("TOML conversion failed: {error}")))?
        }
        _ => {
            return Err(codec_error(
                "internal codec error: expected document format",
            ))
        }
    };

    json_value_to_runtime(json).map(|value| vec![value])
}

fn serialize_document(format: StructuredFormat, values: Vec<Value>) -> Result<Vec<u8>, SparError> {
    let value = match values.len() {
        0 => Value::List(Vec::new()),
        1 => values.into_iter().next().expect("single buffered value"),
        _ => Value::List(values),
    };
    let json = runtime_value_to_json(&value)?;
    match format {
        // A document ends with a newline, like the line-oriented formats, so
        // whatever follows it in a terminal starts on its own line.
        StructuredFormat::Json => serde_json::to_vec(&json)
            .map(|mut bytes| {
                bytes.push(b'\n');
                bytes
            })
            .map_err(|error| codec_error(format!("JSON encoding failed: {error}"))),
        StructuredFormat::Yaml => serde_yaml::to_string(&json)
            .map(String::into_bytes)
            .map_err(|error| codec_error(format!("YAML encoding failed: {error}"))),
        StructuredFormat::Toml => toml::to_string(&json)
            .map(String::into_bytes)
            .map_err(|error| codec_error(format!("TOML encoding failed: {error}"))),
        _ => Err(codec_error(
            "internal codec error: expected document format",
        )),
    }
}

fn drain_lines(pending: &mut Vec<u8>, finish: bool) -> Result<Vec<Value>, SparError> {
    let mut output = Vec::new();
    while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
        let mut line = pending.drain(..=index).collect::<Vec<_>>();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        output.push(Value::String(decode_utf8(line, "line")?));
    }
    if finish && !pending.is_empty() {
        let line = std::mem::take(pending);
        output.push(Value::String(decode_utf8(line, "line")?));
    }
    Ok(output)
}

fn drain_json_lines(pending: &mut Vec<u8>, finish: bool) -> Result<Vec<Value>, SparError> {
    let mut output = Vec::new();
    while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
        let mut line = pending.drain(..=index).collect::<Vec<_>>();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        output.push(parse_json_line(&line)?);
    }
    if finish && !pending.iter().all(u8::is_ascii_whitespace) {
        let line = std::mem::take(pending);
        output.push(parse_json_line(&line)?);
    }
    Ok(output)
}

fn parse_json_line(bytes: &[u8]) -> Result<Value, SparError> {
    let parsed = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|error| codec_error(format!("invalid JSONL record: {error}")))?;
    json_value_to_runtime(parsed)
}

fn drain_delimited(
    pending: &mut Vec<u8>,
    delimiter: u8,
    headers: &mut Option<Vec<String>>,
    finish: bool,
) -> Result<Vec<Value>, SparError> {
    let mut output = Vec::new();
    while let Some(end) = find_delimited_record_end(pending) {
        let record = pending.drain(..end).collect::<Vec<_>>();
        if record
            .iter()
            .all(|byte| matches!(*byte, b'\r' | b'\n' | b' ' | b'\t'))
        {
            continue;
        }
        if let Some(value) = consume_delimited_record(&record, delimiter, headers)? {
            output.push(value);
        }
    }

    if finish && !pending.is_empty() {
        if record_has_unclosed_quote(pending) {
            return Err(codec_error(format!(
                "{} input ended inside a quoted field",
                if delimiter == b',' { "CSV" } else { "TSV" }
            )));
        }
        let record = std::mem::take(pending);
        if !record.iter().all(|byte| byte.is_ascii_whitespace()) {
            if let Some(value) = consume_delimited_record(&record, delimiter, headers)? {
                output.push(value);
            }
        }
    }

    Ok(output)
}

fn consume_delimited_record(
    bytes: &[u8],
    delimiter: u8,
    headers: &mut Option<Vec<String>>,
) -> Result<Option<Value>, SparError> {
    let cells = parse_delimited_record(bytes, delimiter)?;
    if headers.is_none() {
        let fields = cells.into_iter().map(|(text, _)| text).collect::<Vec<_>>();
        if fields.is_empty() || fields.iter().any(String::is_empty) {
            return Err(codec_error(
                "delimited input requires non-empty header names",
            ));
        }
        let unique = fields.iter().collect::<HashSet<_>>();
        if unique.len() != fields.len() {
            return Err(codec_error(
                "delimited input contains duplicate header names",
            ));
        }
        *headers = Some(fields);
        return Ok(None);
    }

    let expected = headers.as_ref().expect("headers initialized");
    if cells.len() != expected.len() {
        return Err(codec_error(format!(
            "delimited record expected {} field(s), found {}",
            expected.len(),
            cells.len()
        )));
    }
    Ok(Some(Value::Object(
        expected
            .iter()
            .cloned()
            .zip(
                cells
                    .into_iter()
                    .map(|(text, quoted)| infer_cell(text, quoted)),
            )
            .collect(),
    )))
}

/// A CSV/TSV cell that reads as a plain integer, decimal, or `true`/`false`
/// becomes that type; anything else (including any quoted cell, and numbers
/// with leading zeros such as `007`) stays text.
fn infer_cell(text: String, quoted: bool) -> Value {
    if quoted {
        return Value::String(text);
    }
    match text.as_str() {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    let digits = text.strip_prefix('-').unwrap_or(&text);
    let plain_integer = !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'));
    if plain_integer {
        if let Ok(number) = text.parse::<i64>() {
            return Value::Int(number);
        }
        return Value::String(text);
    }
    if let Some((whole, fraction)) = digits.split_once('.') {
        let whole_ok = whole == "0" || (!whole.is_empty() && !whole.starts_with('0'));
        let fraction_ok =
            !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit());
        if whole_ok && fraction_ok {
            if let Ok(number) = text.parse::<f64>() {
                return Value::Float(number);
            }
        }
    }
    Value::String(text)
}

fn find_delimited_record_end(bytes: &[u8]) -> Option<usize> {
    let mut in_quotes = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' if in_quotes && bytes.get(index + 1) == Some(&b'"') => {
                index += 2;
                continue;
            }
            b'"' => in_quotes = !in_quotes,
            b'\n' if !in_quotes => return Some(index + 1),
            _ => {}
        }
        index += 1;
    }
    None
}

fn record_has_unclosed_quote(bytes: &[u8]) -> bool {
    let mut in_quotes = false;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            if in_quotes && bytes.get(index + 1) == Some(&b'"') {
                index += 2;
                continue;
            }
            in_quotes = !in_quotes;
        }
        index += 1;
    }
    in_quotes
}

fn parse_delimited_record(bytes: &[u8], delimiter: u8) -> Result<Vec<(String, bool)>, SparError> {
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    let bytes = &bytes[..end];

    let mut fields: Vec<(String, bool)> = Vec::new();
    let mut field = Vec::new();
    let mut was_quoted = false;
    let mut in_quotes = false;
    let mut after_quote = false;
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_quotes {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    field.push(b'"');
                    index += 2;
                    continue;
                }
                in_quotes = false;
                after_quote = true;
            } else {
                field.push(byte);
            }
            index += 1;
            continue;
        }

        if after_quote {
            if byte == delimiter {
                fields.push((
                    decode_utf8(std::mem::take(&mut field), "delimited field")?,
                    std::mem::take(&mut was_quoted),
                ));
                after_quote = false;
                index += 1;
                continue;
            }
            return Err(codec_error(
                "unexpected characters after closing quote in delimited record",
            ));
        }

        if byte == delimiter {
            fields.push((
                decode_utf8(std::mem::take(&mut field), "delimited field")?,
                std::mem::take(&mut was_quoted),
            ));
        } else if byte == b'"' {
            if !field.is_empty() {
                return Err(codec_error(
                    "quote must begin at the start of a delimited field",
                ));
            }
            in_quotes = true;
            was_quoted = true;
        } else {
            field.push(byte);
        }
        index += 1;
    }

    if in_quotes {
        return Err(codec_error("delimited record ended inside a quoted field"));
    }
    fields.push((decode_utf8(field, "delimited field")?, was_quoted));
    Ok(fields)
}

fn decode_utf8(bytes: Vec<u8>, label: &str) -> Result<String, SparError> {
    String::from_utf8(bytes)
        .map_err(|error| codec_error(format!("{label} is not valid UTF-8: {error}")))
}

fn encode_json_lines(value: &Value) -> Result<Vec<u8>, SparError> {
    let values: Vec<&Value> = match value {
        Value::Table(table) => table.rows().iter().collect(),
        Value::List(values) => values.iter().collect(),
        _ => vec![value],
    };
    let mut output = Vec::new();
    for value in values {
        let encoded = serde_json::to_vec(&runtime_value_to_json(value)?)
            .map_err(|error| codec_error(format!("JSONL encoding failed: {error}")))?;
        output.extend_from_slice(&encoded);
        output.push(b'\n');
    }
    Ok(output)
}

fn encode_text(value: &Value) -> Result<Vec<u8>, SparError> {
    match value {
        Value::String(text) => Ok(text.as_bytes().to_vec()),
        other => Err(codec_error(format!(
            "text serializer expects str, found {}",
            other.type_name()
        ))),
    }
}

fn encode_lines(value: &Value) -> Result<Vec<u8>, SparError> {
    let values: Vec<&Value> = match value {
        Value::List(values) => values.iter().collect(),
        _ => vec![value],
    };
    let mut output = Vec::new();
    for value in values {
        let Value::String(text) = value else {
            return Err(codec_error(format!(
                "lines serializer expects str, found {}",
                value.type_name()
            )));
        };
        output.extend_from_slice(text.as_bytes());
        output.push(b'\n');
    }
    Ok(output)
}

fn encode_delimited(
    value: &Value,
    delimiter: u8,
    headers: &mut Option<Vec<String>>,
) -> Result<Vec<u8>, SparError> {
    match value {
        Value::Table(table) => {
            let mut output = Vec::new();
            for row in table.rows() {
                output.extend(encode_delimited(row, delimiter, headers)?);
            }
            Ok(output)
        }
        Value::List(values) => {
            let mut output = Vec::new();
            for row in values {
                output.extend(encode_delimited(row, delimiter, headers)?);
            }
            Ok(output)
        }
        Value::Object(fields) => encode_delimited_row(fields, delimiter, headers),
        other => Err(codec_error(format!(
            "{} serializer expects Record rows, found {}",
            if delimiter == b',' { "CSV" } else { "TSV" },
            other.type_name()
        ))),
    }
}

fn encode_delimited_row(
    fields: &indexmap::IndexMap<String, Value>,
    delimiter: u8,
    headers: &mut Option<Vec<String>>,
) -> Result<Vec<u8>, SparError> {
    let mut output = Vec::new();
    let ordered = if let Some(existing) = headers.as_ref() {
        let actual = fields.keys().cloned().collect::<HashSet<_>>();
        let expected = existing.iter().cloned().collect::<HashSet<_>>();
        if actual != expected {
            return Err(codec_error("delimited rows must use the same field set"));
        }
        existing.clone()
    } else {
        let mut names = fields.keys().cloned().collect::<Vec<_>>();
        names.sort();
        if names.is_empty() {
            return Err(codec_error(
                "delimited rows must contain at least one field",
            ));
        }
        output.extend(encode_delimited_text_record(&names, delimiter));
        *headers = Some(names.clone());
        names
    };

    let values = ordered
        .iter()
        .map(|name| scalar_to_text(fields.get(name).expect("field set validated")))
        .collect::<Result<Vec<_>, _>>()?;
    output.extend(encode_delimited_text_record(&values, delimiter));
    Ok(output)
}

fn scalar_to_text(value: &Value) -> Result<String, SparError> {
    match value {
        Value::Void => Ok(String::new()),
        Value::Int(value) => Ok(value.to_string()),
        Value::Float(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::String(value) => Ok(value.clone()),
        other => Err(codec_error(format!(
            "delimited serializer cannot encode nested {} value",
            other.type_name()
        ))),
    }
}

fn encode_delimited_text_record(fields: &[String], delimiter: u8) -> Vec<u8> {
    let mut output = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push(delimiter);
        }
        let requires_quotes = field
            .as_bytes()
            .iter()
            .any(|byte| matches!(*byte, b'"' | b'\n' | b'\r') || *byte == delimiter);
        if requires_quotes {
            output.push(b'"');
            for byte in field.as_bytes() {
                if *byte == b'"' {
                    output.extend_from_slice(b"\"\"");
                } else {
                    output.push(*byte);
                }
            }
            output.push(b'"');
        } else {
            output.extend_from_slice(field.as_bytes());
        }
    }
    output.push(b'\n');
    output
}

fn format_name(format: StructuredFormat) -> &'static str {
    match format {
        StructuredFormat::Json => "JSON",
        StructuredFormat::JsonLines => "JSONL",
        StructuredFormat::Csv => "CSV",
        StructuredFormat::Tsv => "TSV",
        StructuredFormat::Lines => "lines",
        StructuredFormat::Yaml => "YAML",
        StructuredFormat::Toml => "TOML",
        StructuredFormat::Text => "text",
    }
}

pub(crate) fn json_value_to_runtime(value: serde_json::Value) -> Result<Value, SparError> {
    match value {
        serde_json::Value::Null => Ok(Value::Void),
        serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = value.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(codec_error("JSON number is outside Spar's numeric range"))
            }
        }
        serde_json::Value::String(value) => Ok(Value::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(json_value_to_runtime)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| json_value_to_runtime(value).map(|value| (key, value)))
            .collect::<Result<indexmap::IndexMap<_, _>, _>>()
            .map(Value::Object),
    }
}

pub(crate) fn runtime_value_to_json(value: &Value) -> Result<serde_json::Value, SparError> {
    match value {
        Value::Void => Ok(serde_json::Value::Null),
        Value::Int(value) => Ok(serde_json::json!(value)),
        Value::Float(value) => Ok(serde_json::json!(value)),
        Value::Bool(value) => Ok(serde_json::json!(value)),
        Value::String(value) => Ok(serde_json::json!(value)),
        Value::Bytes(value) => Ok(serde_json::Value::Array(
            value.iter().map(|value| serde_json::json!(value)).collect(),
        )),
        Value::List(values) => values
            .iter()
            .map(runtime_value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| runtime_value_to_json(value).map(|value| (key.clone(), value)))
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(serde_json::Value::Object),
        Value::Map(entries) => entries
            .iter()
            .map(|(key, value)| {
                let Value::String(key) = key else {
                    return Err(codec_error("JSON object map keys must be str"));
                };
                runtime_value_to_json(value).map(|value| (key.clone(), value))
            })
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(serde_json::Value::Object),
        Value::Table(table) => table
            .rows()
            .iter()
            .map(runtime_value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        Value::Schema(schema) => Ok(serde_json::Value::Array(
            schema
                .fields
                .iter()
                .map(|field| {
                    serde_json::json!({
                        "name": field.name,
                        "type": field.ty.display_name(),
                        "optional": field.optional,
                    })
                })
                .collect(),
        )),
        Value::Error {
            message,
            kind,
            code,
            cause,
        } => {
            let mut object = serde_json::Map::new();
            object.insert("message".into(), serde_json::Value::String(message.clone()));
            object.insert("kind".into(), serde_json::Value::String(kind.clone()));
            object.insert("code".into(), serde_json::json!(code));
            if let Some(cause) = cause {
                object.insert("cause".into(), runtime_value_to_json(cause)?);
            }
            Ok(serde_json::Value::Object(object))
        }
        Value::Option(_)
        | Value::Result(_)
        | Value::Shell(_)
        | Value::MixedShell(_)
        | Value::ShellProgram(_)
        | Value::Promise(_)
        | Value::Resource(_)
        | Value::Closure(_)
        | Value::Function(_) => Err(codec_error(format!(
            "{} cannot be encoded as structured data",
            value.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_csv_record_waits_for_newline_outside_quotes() {
        assert_eq!(find_delimited_record_end(b"a,\"b\nc\""), None);
        assert_eq!(find_delimited_record_end(b"a,\"b\nc\"\nnext"), Some(8));
    }

    #[test]
    fn delimited_record_parser_decodes_escaped_quotes() {
        assert_eq!(
            parse_delimited_record(b"Obi,\"hello \"\"world\"\"\"\n", b',')
                .unwrap()
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>(),
            vec!["Obi", "hello \"world\""]
        );
    }
}
