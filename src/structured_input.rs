use crate::error::{Span, SparError};
use crate::runtime::Value;
use crate::structured_codec::{
    CodecDescriptor, CodecMode, StructuredFormat, StructuredFormatRegistry,
};

pub use crate::ast::DecoderNamespace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderKind {
    Codec,
    Scoc,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderOutputShape {
    Scalar,
    Record,
    List,
    Table,
}

impl DecoderOutputShape {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "Scalar",
            Self::Record => "Record",
            Self::List => "List",
            Self::Table => "Table",
        }
    }
}

impl From<scoc::OutputShape> for DecoderOutputShape {
    fn from(value: scoc::OutputShape) -> Self {
        match value {
            scoc::OutputShape::Scalar => Self::Scalar,
            scoc::OutputShape::Record => Self::Record,
            scoc::OutputShape::List => Self::List,
            scoc::OutputShape::Table => Self::Table,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderOptionKind {
    Bool,
    Integer,
    Float,
    String,
    Enum,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecoderOptionSpec {
    pub name: String,
    pub kind: DecoderOptionKind,
    pub description: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecoderCapabilities {
    pub raw: bool,
    pub streaming: bool,
    pub ignore_errors: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecoderDescriptor {
    pub kind: DecoderKind,
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub normalized_output: DecoderOutputShape,
    pub raw_output: Option<DecoderOutputShape>,
    pub stream_item: Option<DecoderOutputShape>,
    pub options: Vec<DecoderOptionSpec>,
    pub capabilities: DecoderCapabilities,
    pub platforms: Vec<String>,
    pub compatibility_alias_for: Option<String>,
    pub jc_baseline: Option<String>,
}

impl DecoderDescriptor {
    pub fn output_shape(&self, raw: bool) -> DecoderOutputShape {
        if raw {
            self.raw_output.unwrap_or(self.normalized_output)
        } else {
            self.normalized_output
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamingMode {
    Auto,
    Enabled,
    Disabled,
}

#[derive(Clone, Debug)]
pub struct ResolvedDecoder {
    pub descriptor: DecoderDescriptor,
    pub canonical_name: String,
    pub forced_streaming: Option<StreamingMode>,
}

#[derive(Clone, Debug)]
pub struct StructuredInputRegistry {
    codecs: StructuredFormatRegistry,
}

impl Default for StructuredInputRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl StructuredInputRegistry {
    pub fn builtin() -> Self {
        Self {
            codecs: StructuredFormatRegistry::builtin(),
        }
    }

    pub fn codecs(&self) -> &StructuredFormatRegistry {
        &self.codecs
    }

    pub fn descriptors(&self) -> Vec<DecoderDescriptor> {
        let mut values = self
            .codecs
            .descriptors()
            .into_iter()
            .map(codec_descriptor)
            .collect::<Vec<_>>();
        values.extend(scoc::parsers().map(scoc_descriptor));
        values.extend(scoc::parsers().filter_map(streaming_alias_descriptor));
        values
    }

    pub fn resolve(
        &self,
        namespace: Option<DecoderNamespace>,
        name: &str,
        span: &Span,
    ) -> Result<ResolvedDecoder, SparError> {
        match namespace {
            Some(DecoderNamespace::Codec) => self.resolve_codec(name, span),
            Some(DecoderNamespace::Scoc) => self.resolve_scoc(name, span),
            Some(DecoderNamespace::Custom) => Err(SparError::TypeError {
                message: format!("custom decoder `{name}` is not registered; custom parser declarations are not implemented in this milestone"),
                hint: None,
                span: span.clone(),
            }),
            None => {
                if self.codecs.descriptor(name).is_some() {
                    self.resolve_codec(name, span)
                } else {
                    self.resolve_scoc(name, span)
                }
            }
        }
    }

    fn resolve_codec(&self, name: &str, span: &Span) -> Result<ResolvedDecoder, SparError> {
        let codec = self
            .codecs
            .descriptor(name)
            .ok_or_else(|| SparError::TypeError {
                message: format!("unknown native codec `{name}`"),
                hint: Some(
                    "use a codec name such as json, jsonl, csv, yaml, toml, lines, or text".into(),
                ),
                span: span.clone(),
            })?;
        Ok(ResolvedDecoder {
            canonical_name: codec.name().to_string(),
            descriptor: codec_descriptor(codec),
            forced_streaming: None,
        })
    }

    fn resolve_scoc(&self, name: &str, span: &Span) -> Result<ResolvedDecoder, SparError> {
        let normalized = name.trim().to_ascii_lowercase();
        if let Some(descriptor) = scoc::parser(&normalized) {
            ensure_scoc_platform(descriptor, span)?;
            // SCOC may register a streaming suffix (`ping-s`) as a plain
            // registry alias for its batch parser (`ping`), so a direct hit
            // here doesn't mean the caller asked for the batch form — check
            // `upstream.streaming_name` the same way the fallback loop below
            // does, and still force streaming on.
            let is_streaming_alias = descriptor
                .upstream
                .and_then(|upstream| upstream.streaming_name)
                .is_some_and(|streaming_name| streaming_name.eq_ignore_ascii_case(&normalized));
            if is_streaming_alias {
                let mut alias = scoc_descriptor(descriptor);
                alias.name = normalized.clone();
                alias.compatibility_alias_for = Some(descriptor.name.to_string());
                return Ok(ResolvedDecoder {
                    canonical_name: descriptor.name.to_string(),
                    descriptor: alias,
                    forced_streaming: Some(StreamingMode::Enabled),
                });
            }
            return Ok(ResolvedDecoder {
                canonical_name: descriptor.name.to_string(),
                descriptor: scoc_descriptor(descriptor),
                forced_streaming: None,
            });
        }
        for descriptor in scoc::parsers().filter(|descriptor| descriptor.capabilities.streaming) {
            if descriptor
                .upstream
                .and_then(|upstream| upstream.streaming_name)
                .is_some_and(|streaming_name| streaming_name.eq_ignore_ascii_case(&normalized))
            {
                ensure_scoc_platform(descriptor, span)?;
                let mut alias = scoc_descriptor(descriptor);
                alias.name = normalized.clone();
                alias.compatibility_alias_for = Some(descriptor.name.to_string());
                return Ok(ResolvedDecoder {
                    canonical_name: descriptor.name.to_string(),
                    descriptor: alias,
                    forced_streaming: Some(StreamingMode::Enabled),
                });
            }
        }
        Err(SparError::TypeError {
            message: format!("unknown SCOC parser `{name}`"),
            hint: Some("use `from <name>` for built-ins, or inspect decoder completion for available parsers".into()),
            span: span.clone(),
        })
    }
}

fn ensure_scoc_platform(descriptor: &scoc::ParserDescriptor, span: &Span) -> Result<(), SparError> {
    let Some(current) = scoc::Platform::current() else {
        return Ok(());
    };
    if descriptor.supports_platform(current) {
        return Ok(());
    }
    Err(SparError::TypeError {
        message: format!(
            "SCOC parser `{}` is not supported on {}; supported platforms: {}",
            descriptor.name,
            current.as_str(),
            descriptor
                .platforms
                .iter()
                .map(|platform| platform.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        hint: None,
        span: span.clone(),
    })
}

fn codec_descriptor(codec: CodecDescriptor) -> DecoderDescriptor {
    let (normalized_output, stream_item) = match codec.format() {
        StructuredFormat::Lines | StructuredFormat::Text => {
            (DecoderOutputShape::Scalar, Some(DecoderOutputShape::Scalar))
        }
        StructuredFormat::Csv | StructuredFormat::Tsv | StructuredFormat::JsonLines => {
            (DecoderOutputShape::Table, Some(DecoderOutputShape::Record))
        }
        StructuredFormat::Json | StructuredFormat::Yaml | StructuredFormat::Toml => {
            (DecoderOutputShape::Record, None)
        }
    };
    DecoderDescriptor {
        kind: DecoderKind::Codec,
        name: codec.name().to_string(),
        aliases: codec
            .aliases()
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        description: format!("native Spar {} decoder", codec.name()),
        normalized_output,
        raw_output: None,
        stream_item,
        options: Vec::new(),
        capabilities: DecoderCapabilities {
            raw: false,
            streaming: codec.mode() == CodecMode::Streaming,
            ignore_errors: false,
        },
        platforms: Vec::new(),
        compatibility_alias_for: None,
        jc_baseline: None,
    }
}

fn option_kind(kind: scoc::OptionKind) -> DecoderOptionKind {
    match kind {
        scoc::OptionKind::Bool => DecoderOptionKind::Bool,
        scoc::OptionKind::Integer => DecoderOptionKind::Integer,
        scoc::OptionKind::Float => DecoderOptionKind::Float,
        scoc::OptionKind::String => DecoderOptionKind::String,
        scoc::OptionKind::Enum(_) => DecoderOptionKind::Enum,
    }
}

fn scoc_descriptor(descriptor: &'static scoc::ParserDescriptor) -> DecoderDescriptor {
    DecoderDescriptor {
        kind: DecoderKind::Scoc,
        name: descriptor.name.to_string(),
        aliases: descriptor
            .aliases
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        description: descriptor.description.to_string(),
        normalized_output: descriptor.output.normalized.into(),
        raw_output: descriptor.output.raw.map(Into::into),
        stream_item: descriptor.output.stream_item.map(Into::into),
        options: descriptor
            .options
            .iter()
            .map(|option| DecoderOptionSpec {
                name: option.name.to_string(),
                kind: option_kind(option.kind),
                description: option.description.to_string(),
            })
            .collect(),
        capabilities: DecoderCapabilities {
            raw: descriptor.capabilities.raw,
            streaming: descriptor.capabilities.streaming,
            ignore_errors: descriptor.capabilities.ignore_errors,
        },
        platforms: descriptor
            .platforms
            .iter()
            .map(|platform| platform.as_str().to_string())
            .collect(),
        compatibility_alias_for: None,
        jc_baseline: Some(scoc::compatibility_baseline().version.to_string()),
    }
}

fn streaming_alias_descriptor(
    descriptor: &'static scoc::ParserDescriptor,
) -> Option<DecoderDescriptor> {
    if !descriptor.capabilities.streaming {
        return None;
    }
    let streaming_name = descriptor.upstream?.streaming_name?;
    let mut alias = scoc_descriptor(descriptor);
    alias.name = streaming_name.to_string();
    alias.aliases.clear();
    alias.compatibility_alias_for = Some(descriptor.name.to_string());
    Some(alias)
}

pub(crate) fn scoc_batch_to_values(
    value: serde_json::Value,
    shape: scoc::OutputShape,
    span: &Span,
) -> Result<Vec<Value>, SparError> {
    match shape {
        scoc::OutputShape::Table => {
            let serde_json::Value::Array(rows) = value else {
                return Err(SparError::EvalError {
                    message: "SCOC parser declared Table output but returned a non-array value"
                        .into(),
                    span: span.clone(),
                });
            };
            rows.into_iter()
                .map(crate::structured_codec::json_value_to_runtime)
                .collect()
        }
        scoc::OutputShape::Scalar | scoc::OutputShape::Record | scoc::OutputShape::List => {
            Ok(vec![crate::structured_codec::json_value_to_runtime(value)?])
        }
    }
}

pub(crate) fn scoc_stream_to_values(
    values: Vec<serde_json::Value>,
) -> Result<Vec<Value>, SparError> {
    values
        .into_iter()
        .map(crate::structured_codec::json_value_to_runtime)
        .collect()
}

pub(crate) fn scoc_error(error: scoc::ScocError, span: &Span) -> SparError {
    SparError::EvalError {
        message: error.to_string(),
        span: span.clone(),
    }
}

pub fn structured_decoder_descriptors() -> Vec<DecoderDescriptor> {
    StructuredInputRegistry::builtin().descriptors()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unqualified_csv_prefers_native_codec() {
        let registry = StructuredInputRegistry::builtin();
        let resolved = registry.resolve(None, "csv", &Span::dummy()).unwrap();
        assert_eq!(resolved.descriptor.kind, DecoderKind::Codec);
    }

    #[test]
    fn unqualified_df_resolves_to_scoc() {
        let registry = StructuredInputRegistry::builtin();
        let resolved = registry.resolve(None, "df", &Span::dummy()).unwrap();
        assert_eq!(resolved.descriptor.kind, DecoderKind::Scoc);
        assert_eq!(resolved.canonical_name, "df");
    }

    #[test]
    fn custom_namespace_is_reserved_but_not_implemented() {
        let registry = StructuredInputRegistry::builtin();
        let error = registry
            .resolve(Some(DecoderNamespace::Custom), "x", &Span::dummy())
            .unwrap_err();
        assert!(error.to_string().contains("not registered"));
    }

    #[test]
    fn ping_s_forces_streaming() {
        let registry = StructuredInputRegistry::builtin();
        let resolved = registry
            .resolve(Some(DecoderNamespace::Scoc), "ping-s", &Span::dummy())
            .unwrap();
        assert_eq!(resolved.canonical_name, "ping");
        assert_eq!(resolved.forced_streaming, Some(StreamingMode::Enabled));
    }
}
