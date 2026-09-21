use spar::{CodecMode, StructuredFormatRegistry, Value};

fn record(fields: &[(&str, Value)]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<indexmap::IndexMap<_, _>>(),
    )
}

#[test]
fn registry_exposes_explicit_builtin_formats_and_modes() {
    let registry = StructuredFormatRegistry::builtin();

    assert_eq!(
        registry.descriptor("json").unwrap().mode(),
        CodecMode::Document
    );
    assert_eq!(
        registry.descriptor("yaml").unwrap().mode(),
        CodecMode::Document
    );
    assert_eq!(
        registry.descriptor("toml").unwrap().mode(),
        CodecMode::Document
    );
    assert_eq!(
        registry.descriptor("jsonl").unwrap().mode(),
        CodecMode::Streaming
    );
    assert_eq!(registry.descriptor("ndjson").unwrap().name(), "jsonl");
    assert_eq!(
        registry.descriptor("csv").unwrap().mode(),
        CodecMode::Streaming
    );
    assert_eq!(
        registry.descriptor("tsv").unwrap().mode(),
        CodecMode::Streaming
    );
    assert_eq!(
        registry.descriptor("lines").unwrap().mode(),
        CodecMode::Streaming
    );
    assert!(registry.descriptor("auto").is_none());
}

#[test]
fn json_document_parser_buffers_until_finish() {
    let registry = StructuredFormatRegistry::builtin();
    let mut parser = registry.parser("json").unwrap();

    assert!(parser.push(br#"{"name":"obi","age":2"#).unwrap().is_empty());
    assert!(parser.push(br#"4}"#).unwrap().is_empty());

    assert_eq!(
        parser.finish().unwrap(),
        vec![record(&[
            ("name", Value::String("obi".into())),
            ("age", Value::Int(24)),
        ])]
    );
}

#[test]
fn jsonl_parser_emits_complete_values_without_waiting_for_eof() {
    let registry = StructuredFormatRegistry::builtin();
    let mut parser = registry.parser("jsonl").unwrap();

    assert!(parser.push(br#"{"id":1"#).unwrap().is_empty());
    let first = parser.push(b"}\n{\"id\":2}").unwrap();
    assert_eq!(first, vec![record(&[("id", Value::Int(1))])]);
    assert_eq!(
        parser.finish().unwrap(),
        vec![record(&[("id", Value::Int(2))])]
    );
}

#[test]
fn lines_parser_preserves_empty_lines_and_handles_crlf_chunks() {
    let registry = StructuredFormatRegistry::builtin();
    let mut parser = registry.parser("lines").unwrap();

    assert_eq!(
        parser.push(b"alpha\r\n\npar").unwrap(),
        vec![Value::String("alpha".into()), Value::String(String::new())]
    );
    assert!(parser.push(b"tial").unwrap().is_empty());
    assert_eq!(
        parser.finish().unwrap(),
        vec![Value::String("partial".into())]
    );
}

#[test]
fn csv_parser_is_incremental_and_respects_quotes_and_embedded_newlines() {
    let registry = StructuredFormatRegistry::builtin();
    let mut parser = registry.parser("csv").unwrap();

    assert!(parser
        .push(b"name,note\n\"Obi\",\"hello,")
        .unwrap()
        .is_empty());
    let rows = parser
        .push(b" world\"\n\"Ada\",\"line one\nline two\"\n")
        .unwrap();

    assert_eq!(
        rows,
        vec![
            record(&[
                ("name", Value::String("Obi".into())),
                ("note", Value::String("hello, world".into())),
            ]),
            record(&[
                ("name", Value::String("Ada".into())),
                ("note", Value::String("line one\nline two".into())),
            ]),
        ]
    );
    assert!(parser.finish().unwrap().is_empty());
}

#[test]
fn csv_parser_rejects_malformed_row_widths_instead_of_padding() {
    let registry = StructuredFormatRegistry::builtin();
    let mut parser = registry.parser("csv").unwrap();

    parser.push(b"name,age\n").unwrap();
    let error = parser.push(b"Obi\n").unwrap_err().to_string();
    assert!(error.contains("expected 2 field(s), found 1"), "{error}");
}

#[test]
fn tsv_parser_uses_tabs_and_infers_plain_numbers() {
    let registry = StructuredFormatRegistry::builtin();
    let mut parser = registry.parser("tsv").unwrap();

    let rows = parser.push(b"name\tage\nObi\t24\n").unwrap();
    assert_eq!(
        rows,
        vec![record(&[
            ("name", Value::String("Obi".into())),
            ("age", Value::Int(24)),
        ])]
    );
}

#[test]
fn yaml_and_toml_document_parsers_use_the_same_runtime_value_model() {
    let registry = StructuredFormatRegistry::builtin();

    let mut yaml = registry.parser("yaml").unwrap();
    yaml.push(b"name: obi\nage: 24\n").unwrap();
    let yaml_value = yaml.finish().unwrap();

    let mut toml = registry.parser("toml").unwrap();
    toml.push(b"name = \"obi\"\nage = 24\n").unwrap();
    let toml_value = toml.finish().unwrap();

    let expected = vec![record(&[
        ("name", Value::String("obi".into())),
        ("age", Value::Int(24)),
    ])];
    assert_eq!(yaml_value, expected);
    assert_eq!(toml_value, expected);
}

#[test]
fn jsonl_serializer_streams_each_value_immediately() {
    let registry = StructuredFormatRegistry::builtin();
    let mut serializer = registry.serializer("jsonl").unwrap();

    let first = serializer.push(&record(&[("id", Value::Int(1))])).unwrap();
    let second = serializer.push(&record(&[("id", Value::Int(2))])).unwrap();

    assert_eq!(String::from_utf8(first).unwrap(), "{\"id\":1}\n");
    assert_eq!(String::from_utf8(second).unwrap(), "{\"id\":2}\n");
    assert!(serializer.finish().unwrap().is_empty());
}

#[test]
fn csv_serializer_writes_deterministic_header_and_escapes_fields() {
    let registry = StructuredFormatRegistry::builtin();
    let mut serializer = registry.serializer("csv").unwrap();

    let first = serializer
        .push(&record(&[
            ("name", Value::String("Obi".into())),
            ("note", Value::String("hello, \"world\"".into())),
        ]))
        .unwrap();
    let second = serializer
        .push(&record(&[
            ("name", Value::String("Ada".into())),
            ("note", Value::String("ok".into())),
        ]))
        .unwrap();

    assert_eq!(
        String::from_utf8(first).unwrap(),
        "name,note\nObi,\"hello, \"\"world\"\"\"\n"
    );
    assert_eq!(String::from_utf8(second).unwrap(), "Ada,ok\n");
}

#[test]
fn lines_serializer_is_strict_about_value_type() {
    let registry = StructuredFormatRegistry::builtin();
    let mut serializer = registry.serializer("lines").unwrap();

    assert_eq!(
        serializer.push(&Value::String("one".into())).unwrap(),
        b"one\n"
    );
    let error = serializer.push(&Value::Int(2)).unwrap_err().to_string();
    assert!(error.contains("lines serializer expects str"), "{error}");
}

#[test]
fn registry_convenience_bridges_decode_bytes_and_encode_values_explicitly() {
    let registry = StructuredFormatRegistry::builtin();
    let decoded = registry.decode_bytes("json", br#"{"ok":true}"#).unwrap();
    assert_eq!(decoded, vec![record(&[("ok", Value::Bool(true))])]);

    let encoded = registry
        .encode_values(
            "jsonl",
            &[
                record(&[("id", Value::Int(1))]),
                record(&[("id", Value::Int(2))]),
            ],
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(encoded).unwrap(),
        "{\"id\":1}\n{\"id\":2}\n"
    );
}

#[test]
fn unknown_formats_fail_explicitly_instead_of_being_guessed() {
    let registry = StructuredFormatRegistry::builtin();
    let error = registry.parser("whatever").unwrap_err().to_string();
    assert!(
        error.contains("unknown structured format 'whatever'"),
        "{error}"
    );
}

#[test]
fn text_codec_reads_the_whole_input_as_one_string_and_writes_it_verbatim() {
    use spar::{StructuredFormatRegistry, Value};
    let registry = StructuredFormatRegistry::builtin();
    let values = registry.decode_bytes("text", b"hello\nworld\n").unwrap();
    assert_eq!(values, vec![Value::String("hello\nworld\n".into())]);
    assert!(registry.decode_bytes("text", b"").unwrap().is_empty());
    let bytes = registry
        .encode_values(
            "text",
            &[Value::String("a".into()), Value::String("b\n".into())],
        )
        .unwrap();
    assert_eq!(bytes, b"ab\n");
    assert!(registry.encode_values("text", &[Value::Int(1)]).is_err());
}

#[test]
fn csv_and_tsv_cells_infer_plain_scalars_and_keep_ambiguous_ones_as_text() {
    use spar::{StructuredFormatRegistry, Value};
    let registry = StructuredFormatRegistry::builtin();
    let rows = registry
        .decode_bytes(
            "csv",
            b"id,zip,price,count,ok,name,quoted\n1,007,2.5,-3,true,Ada,\"42\"\n",
        )
        .unwrap();
    let Value::Object(row) = &rows[0] else {
        panic!("expected a record");
    };
    assert_eq!(row["id"], Value::Int(1));
    assert_eq!(row["zip"], Value::String("007".into()));
    assert_eq!(row["price"], Value::Float(2.5));
    assert_eq!(row["count"], Value::Int(-3));
    assert_eq!(row["ok"], Value::Bool(true));
    assert_eq!(row["name"], Value::String("Ada".into()));
    assert_eq!(row["quoted"], Value::String("42".into()));

    let tsv = registry.decode_bytes("tsv", b"n\tt\n5\tx\n").unwrap();
    let Value::Object(row) = &tsv[0] else {
        panic!("expected a record");
    };
    assert_eq!(row["n"], Value::Int(5));
}
