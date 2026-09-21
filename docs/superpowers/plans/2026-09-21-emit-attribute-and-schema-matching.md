# `#[emit]` Attribute and Schema Matching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Nothing is emitted by `spar emit` unless a top-level struct or var carries `#[emit]`; schemas use the new `schema Name { ... };` syntax and match importing-file structs by name.

**Architecture:** A new `#[` token and an `Attribute` AST node are attached to top-level `Var`/`Section` declarations by the parser (unknown names rejected there). The resolver records an `emit` flag in `GlobalEntry::Var` and `SectionEntry`; `emit::build_emit_json` emits only flagged items and errors when none are flagged. Schema syntax is replaced in the parser/formatter; `loader::validate_schema_imports` drops the "undeclared section" rule, matches private structs too, and reports duplicate schema names.

**Tech Stack:** Rust (spar, spar-ls crates), tree-sitter grammar, VS Code TextMate grammar, wasm-pack for the playground.

**Spec:** `spar/docs/superpowers/specs/2026-09-21-emit-attribute-and-schema-matching-design.md`

## Global Constraints

- Work in `/home/occ/Projects/Rust/occ_lang/spar` (crate `spar`); spar-ls in `/home/occ/Projects/Rust/occ_lang/spar-ls`. Run cargo from inside each crate dir.
- Only the attribute name `emit` is valid. Valid targets: top-level `struct`, `[Section]`, `var` (including with `export`/`private`). Anything else is an error.
- `export`/`private` no longer affect emit; `export` still means importable from other files.
- Emitted output keys stay sorted alphabetically at every level (`emit::sort_keys` already does this; do not remove it).
- No files with no `#[emit]` items may print `{}`: `spar emit` exits non-zero with exactly `nothing to emit: mark top-level structs or vars with #[emit]`.
- serde API (`spar::from_str`, `spar::from_eval`) is NOT gated by `#[emit]`.
- Schema syntax is `schema Name { fields };`, `schema? Name { fields };`, `schema Name from TypeName;`. `@SchemaFile` and `Schema [Name]{}` / `SchemaFrom [..]` are removed and must produce the targeted errors below.
- Removal error texts (exact): ``@SchemaFile was removed; declare `schema Name { ... };` `` and ``Schema [Name]{...} was replaced by `schema Name { ... };` ``.
- Version bump: `spar` and `spar-ls` to 0.6.0 together (Task 8).
- Never add attribution lines/co-author trailers to commits. The repos have unrelated uncommitted changes: always `git add` explicit paths and `git commit -- <paths>`, never `git add -A`/`git commit -a`.

---

### Task 1: `#[` token, `Attribute` AST node, parser, formatter

**Files:**
- Modify: `spar/src/token.rs` (add `HashBracket` variant + `human_name`)
- Modify: `spar/src/lexer.rs` (new match arm near `b'@'`, ~line 1361; unit test near line 3008)
- Modify: `spar/src/ast.rs` (`Attribute`, `KNOWN_ATTRIBUTES`, `attributes` on `VarDecl` ~line 415 and `SectionDecl` ~line 434)
- Modify: `spar/src/parser.rs` (dispatch ~line 340; `VarDecl {` sites at ~632 and ~2556; `SectionDecl {` sites at ~888, ~1116, ~1297; `parse_section_item`)
- Modify: `spar/src/loader.rs:1355` and `spar/src/formatter.rs:2905` (`SectionDecl {` literals: add `attributes: Vec::new()`)
- Modify: `spar/src/formatter.rs` (`item_span_line`, `format_top_level_item` for `Var`/`Section`)
- Test: `spar/src/tests/parser_tests.rs`, `spar/src/formatter.rs` test module

**Interfaces:**
- Produces: `spar::ast::Attribute { pub name: String, pub span: Span }`; `spar::ast::KNOWN_ATTRIBUTES: &[&str] = &["emit"]`; `VarDecl.attributes: Vec<Attribute>`; `SectionDecl.attributes: Vec<Attribute>`; `VarDecl::is_emit(&self) -> bool`; `SectionDecl::is_emit(&self) -> bool`; `Token::HashBracket`.

- [ ] **Step 1: Write failing lexer + parser tests**

Append to `spar/src/lexer.rs` test module (next to `lex_at_sign`):

```rust
    #[test]
    fn lex_hash_bracket() {
        let tokens = Lexer::new("#[emit]").tokenize().unwrap();
        assert_eq!(tokens[0].token, Token::HashBracket);
        assert_eq!(tokens[1].token, Token::Ident("emit".to_string()));
        assert_eq!(tokens[2].token, Token::RBracket);
    }

    #[test]
    fn bare_hash_is_still_a_lex_error() {
        assert!(Lexer::new("#x").tokenize().is_err());
    }
```

Append to `spar/src/tests/parser_tests.rs`:

```rust
#[test]
fn emit_attribute_attaches_to_struct_and_var() {
    use crate::ast::TopLevelItem;
    let program = parse_ok(
        "#[emit]\nstruct Server { port: int = 1; };\n#[emit]\nvar version: str = \"1\";\nstruct Plain { a: int = 1; };\n",
    );
    let TopLevelItem::Section(server) = &program.items[0] else { panic!("section") };
    assert!(server.is_emit());
    let TopLevelItem::Var(version) = &program.items[1] else { panic!("var") };
    assert!(version.is_emit());
    let TopLevelItem::Section(plain) = &program.items[2] else { panic!("section") };
    assert!(!plain.is_emit());
}

#[test]
fn emit_attribute_works_with_export_and_private_and_stacking() {
    use crate::ast::TopLevelItem;
    let program = parse_ok("#[emit]\n#[emit]\nexport var a: int = 1;\n#[emit]\nprivate struct B { x: int = 1; };\n");
    let TopLevelItem::Var(a) = &program.items[0] else { panic!("var") };
    assert_eq!(a.attributes.len(), 2);
    let TopLevelItem::Section(b) = &program.items[1] else { panic!("section") };
    assert!(b.is_emit() && b.private);
}

#[test]
fn unknown_attribute_is_rejected_with_valid_names() {
    let err = parse_ok_result("#[serialize]\nstruct A { x: int = 1; };").unwrap_err();
    let text = err.to_string();
    assert!(text.contains("unknown attribute `#[serialize]`"), "{text}");
    assert!(text.contains("valid attributes: emit"), "{text}");
}

#[test]
fn attribute_on_function_is_rejected() {
    let err = parse_ok_result("#[emit]\nfunction f() -> int { return 1; };").unwrap_err();
    assert!(
        err.to_string().contains("only valid on top-level structs and vars"),
        "{err}"
    );
}

#[test]
fn dangling_attribute_at_end_of_file_is_rejected() {
    let err = parse_ok_result("var a: int = 1;\n#[emit]\n").unwrap_err();
    assert!(
        err.to_string().contains("only valid on top-level structs and vars"),
        "{err}"
    );
}

#[test]
fn attribute_on_field_is_rejected() {
    let err = parse_ok_result("struct A {\n    #[emit]\n    x: int = 1;\n};").unwrap_err();
    assert!(
        err.to_string().contains("only valid on top-level structs and vars"),
        "{err}"
    );
}
```

`parse_ok_result` already exists in that file (used at ~line 613). If its return type is `Result<Program, SparError>`, `.unwrap_err().to_string()` works; confirm with `grep -n "fn parse_ok_result" spar/src/tests/parser_tests.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd spar && cargo test lex_hash_bracket emit_attribute unknown_attribute attribute_on dangling_attribute bare_hash 2>&1 | tail -20`
Expected: compile errors (`Token::HashBracket` / `is_emit` not found).

- [ ] **Step 3: Implement token, AST, lexer**

`spar/src/token.rs`: add after `At, // '@'`:

```rust
    HashBracket, // `#[` opens an attribute
```
and in `human_name` after `Token::At => "'@'",`:
```rust
            Token::HashBracket => "'#['",
```

`spar/src/lexer.rs`: add this arm immediately before the `b'@' => {` arm:

```rust
            b'#' if self.peek_at(1) == Some(b'[') => {
                self.advance();
                self.advance();
                Token::HashBracket
            }
```

`spar/src/ast.rs`: add near `VarDecl`:

```rust
/// A `#[name]` attribute attached to a top-level declaration.
#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub span: Span,
}

/// The only attribute names the parser accepts.
pub const KNOWN_ATTRIBUTES: &[&str] = &["emit"];

fn has_attribute(attributes: &[Attribute], name: &str) -> bool {
    attributes.iter().any(|attribute| attribute.name == name)
}
```
Add `pub attributes: Vec<Attribute>,` as the last field of `VarDecl` and `SectionDecl`, then:

```rust
impl VarDecl {
    pub fn is_emit(&self) -> bool {
        has_attribute(&self.attributes, "emit")
    }
}

impl SectionDecl {
    pub fn is_emit(&self) -> bool {
        has_attribute(&self.attributes, "emit")
    }
}
```

- [ ] **Step 4: Implement parser**

In `spar/src/parser.rs` add `attributes: Vec::new(),` to every `VarDecl {` and `SectionDecl {` literal (parser.rs ~632, ~2556, ~888, ~1116, ~1297; loader.rs:1355; formatter.rs:2905). Add `Attribute, KNOWN_ATTRIBUTES` to the `use crate::ast::...` import at the top of parser.rs.

Rename the existing `fn parse_top_level_item` to `fn parse_unattributed_top_level_item` and add above it:

```rust
    fn parse_top_level_item(&mut self) -> Result<TopLevelItem, SparError> {
        if !self.at(&Token::HashBracket) {
            return self.parse_unattributed_top_level_item();
        }
        let attributes = self.parse_attributes()?;
        let placement_error = |attribute: &Attribute| SparError::ParseError {
            message: format!(
                "attribute `#[{}]` is only valid on top-level structs and vars",
                attribute.name
            ),
            span: attribute.span.clone(),
        };
        if self.at(&Token::Eof) {
            return Err(placement_error(&attributes[0]));
        }
        let mut item = self.parse_unattributed_top_level_item()?;
        match &mut item {
            TopLevelItem::Var(declaration) => declaration.attributes = attributes,
            TopLevelItem::Section(declaration) => declaration.attributes = attributes,
            _ => return Err(placement_error(&attributes[0])),
        }
        Ok(item)
    }

    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, SparError> {
        let mut attributes = Vec::new();
        while self.at(&Token::HashBracket) {
            let span = self.peek_span();
            self.advance();
            let (name, name_span) = self.expect_ident()?;
            if !KNOWN_ATTRIBUTES.contains(&name.as_str()) {
                return Err(SparError::ParseError {
                    message: format!(
                        "unknown attribute `#[{name}]`; valid attributes: {}",
                        KNOWN_ATTRIBUTES.join(", ")
                    ),
                    span: name_span,
                });
            }
            self.expect(&Token::RBracket)?;
            attributes.push(Attribute { name, span });
        }
        Ok(attributes)
    }
```

Add as the first statement of `fn parse_section_item`:

```rust
        if self.at(&Token::HashBracket) {
            return Err(self.error(
                "attribute `#[...]` is only valid on top-level structs and vars",
            ));
        }
```

- [ ] **Step 5: Run parser/lexer tests**

Run: `cd spar && cargo test lex_hash_bracket emit_attribute unknown_attribute attribute_on dangling_attribute bare_hash 2>&1 | tail -20`
Expected: all PASS.

- [ ] **Step 6: Write failing formatter tests**

Append to the `formatter.rs` test module:

```rust
    #[test]
    fn formatter_keeps_attributes_on_their_own_line_above_the_declaration() {
        let src = "#[emit]   export var  a:int=1;\n#[emit]\nstruct S { x: int = 1; };\n";
        let formatted = format_source(src).unwrap();
        assert_eq!(
            formatted,
            "#[emit]\nexport var a: int = 1;\n\n#[emit]\nstruct S {\n    x: int = 1;\n};\n"
        );
        assert_eq!(format_source(&formatted).unwrap(), formatted, "idempotent");
    }

    #[test]
    fn formatter_places_leading_comment_before_the_attribute() {
        let src = "// the server\n#[emit]\nstruct S { x: int = 1; };\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.starts_with("// the server\n#[emit]\nstruct S"), "{formatted}");
    }
```

Run: `cd spar && cargo test formatter_keeps_attributes formatter_places_leading 2>&1 | tail -15` → Expected: FAIL (attributes dropped). If the existing struct formatting or blank-line rule yields a different but sensible layout than the literal above, adjust the expected string to what `format_source` produces for the same input *without* the attribute lines (run that first), plus the attribute lines; the point is attributes stay attached and the result is idempotent.

- [ ] **Step 7: Implement formatter**

In `formatter.rs`, add helper:

```rust
fn format_attributes(attributes: &[crate::ast::Attribute], out: &mut String) {
    for attribute in attributes {
        out.push_str("#[");
        out.push_str(&attribute.name);
        out.push_str("]\n");
    }
}
```
Call `format_attributes(&vd.attributes, out);` at the very start of the `TopLevelItem::Var(vd)` arm and `format_attributes(&sd.attributes, out);` at the start of the `TopLevelItem::Section(sd)` arm (before the `export` check). In `item_span_line`, make the two arms use the first attribute's line when present:

```rust
        TopLevelItem::Var(d) => d.attributes.first().map_or(d.span.line, |a| a.span.line),
        TopLevelItem::Section(d) => d.attributes.first().map_or(d.span.line, |a| a.span.line),
```
(Leading comments are keyed on this line by `cx.emit_before_line`, so they now print above the attribute.)

- [ ] **Step 8: Run full spar tests**

Run: `cd spar && cargo test 2>&1 | grep -E "test result|FAILED|error\[" | head`
Expected: all PASS (nothing consumes `attributes` yet).

- [ ] **Step 9: Commit**

```bash
cd spar && git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/loader.rs src/formatter.rs src/tests/parser_tests.rs
git commit -m "feat: parse #[emit] attributes on top-level structs and vars" -- src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/loader.rs src/formatter.rs src/tests/parser_tests.rs
```

---

### Task 2: Emit gating

**Files:**
- Modify: `spar/src/resolver.rs` (`GlobalEntry::Var` ~line 134, `SectionEntry` ~line 148, `register_var` ~1323, section registration ~1425 and nested ~1539)
- Modify: `spar/src/compiler.rs:329` (interactive `_` entry)
- Modify: `spar/src/emit.rs` (`build_emit_json`, tests)
- Modify: `spar/src/loader.rs` `localize_visibility` (~line 339)
- Test: `spar/src/emit.rs` test module, `spar/tests/cli_golden.rs`

**Interfaces:**
- Consumes: `VarDecl::is_emit`, `SectionDecl::is_emit` (Task 1).
- Produces: `GlobalEntry::Var { emit: bool, .. }`, `SectionEntry.emit: bool`; `build_emit_json` returns `Err("nothing to emit: mark top-level structs or vars with #[emit]")` when no top-level item is marked.

- [ ] **Step 1: Write failing emit tests**

Replace the `const SRC` used by the existing emit tests and add tests in `emit.rs`'s test module:

```rust
    const SRC: &str = r#"
#[emit]
var name: str = "spar";

#[emit]
struct Server {
    port: int = 8080;
};
"#;

    #[test]
    fn only_emit_marked_items_are_emitted() {
        let json = emit_to_json(
            "#[emit]\nstruct A { x: int = 1; };\nstruct B { y: int = 2; };\n#[emit]\nvar c: int = 3;\nvar d: int = 4;\nexport var e: int = 5;\n",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value, serde_json::json!({"A": {"x": 1}, "c": 3}));
    }

    #[test]
    fn export_var_without_emit_does_not_leak_env_values() {
        let json = emit_to_json(
            "#[emit]\nstruct Ok { a: int = 1; };\nexport var secret: str = env(\"HOME\") ?? \"x\";\n",
        )
        .unwrap();
        assert!(!json.contains("secret"), "{json}");
    }

    #[test]
    fn private_does_not_block_emit_when_marked() {
        let json = emit_to_json("#[emit]\nprivate struct P { x: int = 1; };\n").unwrap();
        assert!(json.contains("\"P\""), "{json}");
    }

    #[test]
    fn nothing_marked_is_an_error() {
        let errors = emit_to_json("struct A { x: int = 1; };\nexport var b: int = 2;\n").unwrap_err();
        assert_eq!(
            errors,
            vec!["nothing to emit: mark top-level structs or vars with #[emit]".to_string()]
        );
    }
```
Also update the existing yaml/toml tests' expectations only if they break (their SRC now carries `#[emit]`; expected strings are unchanged: `"Server:\n  port: 8080\nname: spar\n"` and `"name = \"spar\"\n\n[Server]\nport = 8080\n"`). The existing `promise_values_cannot_be_emitted` test source must become `"async function value() -> int { return 1; }; #[emit]\nvar pending: Promise<int> = value();"` (drop `export`, add `#[emit]`).

Run: `cd spar && cargo test --lib emit:: 2>&1 | tail -20` → Expected: FAIL.

- [ ] **Step 2: Add `emit` to symbol entries**

`resolver.rs`: add `emit: bool,` to `GlobalEntry::Var` (after `mutable`) and to `SectionEntry` (after `private`). In `register_var` add `emit: decl.is_emit(),`. In the top-level section registration (~1425) add `emit: decl.is_emit(),`; in the nested registration (~1539) add `emit: false,`. In `compiler.rs:329` add `emit: false,`.

Run: `cd spar && cargo build 2>&1 | grep -E "^error" -A6 | head -40`; fix any other exhaustive `GlobalEntry::Var {` / `SectionEntry {` literals the compiler reports (grep showed none besides these).

- [ ] **Step 3: Rewrite `build_emit_json`'s selection**

In `emit.rs`, replace the "Exported globals only" block and the section loop with:

```rust
    const NOTHING_TO_EMIT: &str = "nothing to emit: mark top-level structs or vars with #[emit]";
    let mut marked = 0usize;

    for (name, value) in &result.globals {
        let emitted = matches!(
            symbols.globals.get(name),
            Some(GlobalEntry::Var { emit: true, .. })
        );
        if emitted {
            marked += 1;
            root.insert(name.clone(), config_value_to_json(value)?);
        }
    }

    // Top-level sections marked #[emit] (path length == 1)
    let mut section_keys: Vec<&Vec<String>> =
        result.sections.keys().filter(|p| p.len() == 1).collect();
    section_keys.sort();

    for path in section_keys {
        let emitted = symbols
            .sections
            .get(path)
            .map(|entry| entry.emit)
            .unwrap_or(false);
        if emitted {
            marked += 1;
            root.insert(path[0].clone(), build_section_value(path, result)?);
        }
    }

    if marked == 0 {
        return Err(NOTHING_TO_EMIT.to_string());
    }
```
Update the doc comment of `build_emit_json` and the module header: "globals and top-level sections marked `#[emit]`". `emit_to_*` already convert `Err(String)` to `Vec<String>` via `.map_err(|error| vec![error])` in `compile_for_emit`.

- [ ] **Step 4: Imported items never auto-emit**

In `loader.rs` `localize_visibility`, clear the emit attribute on imported declarations:

```rust
        TopLevelItem::Var(mut v) => {
            v.exported = false;
            v.attributes.retain(|attribute| attribute.name != "emit");
            TopLevelItem::Var(v)
        }
        TopLevelItem::Section(mut s) => {
            s.exported = false;
            s.private = true;
            s.attributes.retain(|attribute| attribute.name != "emit");
            TopLevelItem::Section(s)
        }
```
Add a loader test near `selectively_imported_section_is_not_checked_against_schema`:

```rust
    #[test]
    fn imported_emit_marked_struct_is_not_emitted_by_the_importer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.spar"),
            "#[emit]\nexport struct Shared { x: int = 1; };\n",
        )
        .unwrap();
        let src = "import { Shared } from \"lib.spar\";\n#[emit]\nstruct Mine { y: int = 2; };\n";
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        for item in &program.items {
            if let TopLevelItem::Section(s) = item {
                if s.path == vec!["Shared".to_string()] {
                    assert!(!s.is_emit(), "imported struct must lose #[emit]");
                }
            }
        }
    }
```

- [ ] **Step 5: Update CLI golden tests**

Open `spar/tests/cli_golden.rs`; the golden sources (`struct Server`, `export var name`) need `#[emit]`. Edit the source constant so both items are marked (`#[emit]\nexport var name: str = "spar";\n\n#[emit]\nstruct Server {...};`), keep the expected JSON/YAML unchanged, and add:

```rust
#[test]
fn emit_without_marked_items_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain.spar");
    std::fs::write(&file, "struct A { x: int = 1; };\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["emit", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nothing to emit: mark top-level structs or vars with #[emit]"),
        "{stderr}"
    );
}
```

- [ ] **Step 6: Run tests**

Run: `cd spar && cargo test --lib emit:: loader:: 2>&1 | tail -20` then `cargo test --test cli_golden 2>&1 | tail -20`
Expected: PASS. Other test files still fail until Task 5; that's expected.

- [ ] **Step 7: Commit**

```bash
cd spar && git add src/resolver.rs src/compiler.rs src/emit.rs src/loader.rs tests/cli_golden.rs
git commit -m "feat: emit only declarations marked #[emit]" -- src/resolver.rs src/compiler.rs src/emit.rs src/loader.rs tests/cli_golden.rs
```

---

### Task 3: New `schema` syntax, removal errors, formatter

**Files:**
- Modify: `spar/src/parser.rs` (`parse` prologue ~line 180, schema-file validation ~219-270, dispatch ~364, `parse_schema_decl`, `parse_schema_from_decl`)
- Modify: `spar/src/formatter.rs` (~79-87 pragma printing, `SchemaSection` ~440, `SchemaFrom` ~532, tests ~2925/3122/3133/3156)
- Modify: `spar/src/loader.rs:1464` (message when the imported file has no schema declarations)
- Test: `spar/src/tests/parser_tests.rs` (replace `parses_schema_file_pragma`, `parses_required_schema_section` and siblings that use `@SchemaFile`/`Schema [X]`)

**Interfaces:**
- Produces: `Program.is_schema_file` is now `true` iff the program contains a `SchemaSection` or `SchemaFrom` item (field kept so callers are unchanged). AST nodes `SchemaSectionDecl`/`SchemaFromDecl` are unchanged; only their surface syntax changes.

- [ ] **Step 1: Write failing parser tests**

Replace the schema tests in `parser_tests.rs` (`parses_schema_file_pragma`, `parses_required_schema_section`, and any other test whose source contains `@SchemaFile` or `Schema [`; find them with `grep -n "SchemaFile\|Schema \[\|SchemaFrom" spar/src/tests/parser_tests.rs`) with:

```rust
#[test]
fn schema_declaration_makes_a_schema_file() {
    let prog = parse_ok("schema X { a: int; };");
    assert!(prog.is_schema_file);
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaSection(s) => {
            assert_eq!(s.name, "X");
            assert!(!s.marker.optional);
            assert_eq!(s.fields.len(), 1);
        }
        other => panic!("expected schema, got {other:?}"),
    }
}

#[test]
fn optional_schema_declaration() {
    let prog = parse_ok("schema? Cache { ttl: int; };");
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaSection(s) => assert!(s.marker.optional),
        other => panic!("expected schema, got {other:?}"),
    }
}

#[test]
fn schema_from_declaration() {
    let prog = parse_ok("schema Db from DbType;");
    match &prog.items[0] {
        crate::ast::TopLevelItem::SchemaFrom(s) => {
            assert_eq!(s.name, "Db");
            assert_eq!(s.source_type, "DbType");
        }
        other => panic!("expected schema-from, got {other:?}"),
    }
}

#[test]
fn schema_file_may_only_contain_schema_items_and_type_imports() {
    let err = parse_ok_result("schema X { a: int; };\nvar y: int = 1;").unwrap_err();
    assert!(err.to_string().contains("schema files may only contain"), "{err}");
}

#[test]
fn old_schema_file_pragma_is_removed_with_a_hint() {
    let err = parse_ok_result("@SchemaFile\nschema X { a: int; };").unwrap_err();
    assert!(
        err.to_string()
            .contains("@SchemaFile was removed; declare `schema Name { ... };`"),
        "{err}"
    );
}

#[test]
fn old_bracket_schema_syntax_is_removed_with_a_hint() {
    let err = parse_ok_result("Schema [X]{ a: int; };").unwrap_err();
    assert!(
        err.to_string()
            .contains("Schema [Name]{...} was replaced by `schema Name { ... };`"),
        "{err}"
    );
}

#[test]
fn a_variable_named_schema_still_works() {
    let prog = parse_ok("var schema: int = 1;");
    assert_eq!(prog.items.len(), 1);
    assert!(!prog.is_schema_file);
}
```

Run: `cd spar && cargo test schema_ 2>&1 | tail -20` → Expected: FAIL.

- [ ] **Step 2: Parser prologue and pragma removal**

In `Parser::parse`, change the pragma match so `"SchemaFile"` errors and `is_schema_file` is computed after items are parsed:

```rust
        let load_env = if self.at(&Token::At) {
            self.advance(); // consume '@'
            let (name, name_span) = self.expect_ident()?;
            match name.as_str() {
                "SchemaFile" => {
                    return Err(SparError::ParseError {
                        message: "@SchemaFile was removed; declare `schema Name { ... };` \
                                  (any file containing `schema` declarations is a schema file)"
                            .to_string(),
                        span: name_span,
                    });
                }
                "LoadEnv" => {
                    let path = if self.at(&Token::LParen) {
                        self.advance();
                        let path = self.parse_load_env_path()?;
                        self.expect(&Token::RParen)?;
                        path
                    } else {
                        ".env".to_string()
                    };
                    Some(path)
                }
                _ => {
                    return Err(SparError::ParseError {
                        message: format!(
                            "unknown file pragma `@{name}`; only `@LoadEnv` is supported"
                        ),
                        span: name_span,
                    });
                }
            }
        } else {
            None
        };

        let mut items = Vec::new();
        loop {
            if self.at(&Token::Eof) {
                break;
            }
            items.push(self.parse_top_level_item_or_expression()?);
        }

        let is_schema_file = items.iter().any(|item| {
            matches!(item, TopLevelItem::SchemaSection(_) | TopLevelItem::SchemaFrom(_))
        });
```
Keep the existing "if is_schema_file { exclusivity } " block, but delete the `else` block that rejected schema declarations in non-schema files (a file with `schema` declarations *is* a schema file now), and change its message to:
``"schema files may only contain `schema Name {...};` declarations, `schema Name from Type;`, and `import type {...} from \"...\";`"``.

Also update the mid-file pragma error near the end of `parse_unattributed_top_level_item` (the `Token::At` arm) to say `'@LoadEnv' pragma must be the first item in the file` and the final catch-all message to list `'schema'` instead of `'Schema'`.

- [ ] **Step 3: Dispatch and declaration parsers**

In `parse_unattributed_top_level_item`, replace the `Ident(s) if s == "Schema"` and `"SchemaFrom"` arms with:

```rust
            Token::Ident(s) if s == "Schema" && self.next_is(&Token::LBracket) => Err(self.error(
                "Schema [Name]{...} was replaced by `schema Name { ... };`",
            )),
            Token::Ident(s) if s == "SchemaFrom" && self.next_is(&Token::LBracket) => Err(self.error(
                "SchemaFrom [Name, Type]; was replaced by `schema Name from Type;`",
            )),
            Token::Ident(s) if s == "schema" && self.schema_declaration_follows() => {
                self.parse_schema_item()
            }
```
Place these arms before the generic `Token::Ident(_) | ...` statement arm. Add:

```rust
    /// `schema Name ...` / `schema? Name ...` — a declaration, not a variable
    /// or call that happens to be named `schema`.
    fn schema_declaration_follows(&self) -> bool {
        match self.tokens.get(self.pos + 1).map(|t| &t.token) {
            Some(Token::Question) => true,
            Some(Token::Ident(_)) => true,
            _ => false,
        }
    }

    /// `schema [?] Name { fields };` or `schema [?] Name from Type;`.
    fn parse_schema_item(&mut self) -> Result<TopLevelItem, SparError> {
        let span = self.peek_span();
        self.advance(); // consume 'schema'
        let optional = if self.at(&Token::Question) {
            self.advance();
            true
        } else {
            false
        };
        let (name, _) = self.expect_ident()?;
        if matches!(self.peek(), Token::Ident(word) if word == "from") {
            self.advance(); // consume 'from'
            let (source_type, source_type_span) = self.expect_ident()?;
            self.expect(&Token::Semicolon)?;
            return Ok(TopLevelItem::SchemaFrom(SchemaFromDecl {
                name,
                source_type,
                source_type_span,
                marker: SchemaMarker { optional },
                span,
            }));
        }
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&Token::RBrace) && !self.at(&Token::Eof) {
            fields.push(self.parse_schema_field()?);
        }
        self.expect(&Token::RBrace)?;
        self.expect(&Token::Semicolon)?;
        Ok(TopLevelItem::SchemaSection(SchemaSectionDecl {
            name,
            marker: SchemaMarker { optional },
            fields,
            span,
        }))
    }
```
Delete the old `parse_schema_decl` and `parse_schema_from_decl` functions (dead code otherwise; Clippy would fail).

- [ ] **Step 4: Loader message**

In `loader.rs` (~line 1464) change the "not a schema file" error to:
``format!("'{}' is not a schema file — declare `schema Name {{ ... }};` in it", decl.path)``.

- [ ] **Step 5: Formatter**

In `formatter.rs`: delete the `if program.is_schema_file { out.push_str("@SchemaFile\n"); }` block and remove `|| program.is_schema_file` from the `needs_separator` expression. Replace the `SchemaSection` and `SchemaFrom` arms:

```rust
        TopLevelItem::SchemaSection(sd) => {
            out.push_str("schema");
            if sd.marker.optional {
                out.push('?');
            }
            out.push(' ');
            out.push_str(&sd.name);
            out.push_str(" {\n");
            for field in &sd.fields {
                format_schema_field(field, 1, config, out);
            }
            out.push_str("};\n");
        }
```
```rust
        TopLevelItem::SchemaFrom(sf) => {
            out.push_str("schema");
            if sf.marker.optional {
                out.push('?');
            }
            out.push(' ');
            out.push_str(&sf.name);
            out.push_str(" from ");
            out.push_str(&sf.source_type);
            out.push_str(";\n");
        }
```
Update the four formatter tests that used `@SchemaFile\nSchema [X]{...}` (~lines 2925, 3122, 3133, 3156): change sources to `schema X {\n    a: int;\n};\n`, `schema? Y {\n    b: str;\n};\n`, etc., and their assertions (`formatted.contains("schema X {")`; delete the assertion that the output starts with `@SchemaFile\n`). Add:

```rust
    #[test]
    fn schema_from_formats_in_new_syntax() {
        let formatted = format_source("schema?   Db   from   DbType;").unwrap();
        assert_eq!(formatted, "schema? Db from DbType;\n");
    }
```

- [ ] **Step 6: Run tests**

Run: `cd spar && cargo test --lib 2>&1 | grep -E "test result|FAILED|panicked" | head -20`
Expected: schema-related loader tests still fail if they contain old syntax (fixed in Task 4/5); parser and formatter tests PASS.

- [ ] **Step 7: Commit**

```bash
cd spar && git add src/parser.rs src/formatter.rs src/loader.rs src/tests/parser_tests.rs
git commit -m "feat: replace @SchemaFile and Schema [Name] with schema Name { ... }" -- src/parser.rs src/formatter.rs src/loader.rs src/tests/parser_tests.rs
```

---

### Task 4: Schema matching semantics

**Files:**
- Modify: `spar/src/loader.rs` (`validate_schema_imports` ~1385-1585; tests ~1868, ~2344, ~2317, ~2546-2650 — convert sources to new syntax)
- Test: `spar/src/loader.rs` test module

**Interfaces:**
- Consumes: new schema syntax (Task 3).
- Produces: `validate_schema_imports` signature unchanged. New behavior: private structs are matched; structs without a schema are ignored; missing required schema → error with optional `did you mean` hint; duplicate schema names across imports → error.

- [ ] **Step 1: Convert existing loader tests and add failing tests**

In `loader.rs` tests, replace every `"@SchemaFile\nSchema [Name]{...}"` schema-file source with `"schema Name {...}"` (`grep -n "SchemaFile" src/loader.rs`), and every `SchemaFrom [A, B];` with `schema A from B;`. Then delete/replace these tests whose premise is gone:
- `private_section_not_validated_against_schema` (~1868) → replace with `private_section_is_validated_against_schema` (below).
- any test asserting the message `is not declared in any imported schema` → replace with `struct_without_schema_is_ignored`.

Add (adapting the existing helper style: `tempfile::tempdir`, `parse_src`, `validate_schema_imports(&program, dir.path())`):

```rust
    fn schema_result(schema_src: &str, config_src: &str) -> Result<
        std::collections::HashMap<String, Vec<crate::ast::SchemaField>>,
        Vec<crate::error::SparError>,
    > {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("s.spar"), schema_src).unwrap();
        let program = parse_src(&format!("import schema \"s.spar\";\n{config_src}"));
        validate_schema_imports(&program, dir.path())
    }

    fn messages(errors: Vec<crate::error::SparError>) -> Vec<String> {
        errors.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn matched_struct_is_type_checked() {
        let errors = schema_result(
            "schema Server { host: str; port: int; };",
            "struct Server { host: str = \"h\"; };",
        )
        .unwrap_err();
        assert!(messages(errors).iter().any(|m| m.contains("port")));
    }

    #[test]
    fn struct_without_schema_is_ignored() {
        assert!(schema_result(
            "schema Server { host: str; };",
            "struct Server { host: str = \"h\"; };\nstruct Other { z: int = 1; };",
        )
        .is_ok());
    }

    #[test]
    fn missing_required_schema_struct_is_an_error() {
        let errors = schema_result("schema Server { host: str; };", "struct Other { z: int = 1; };")
            .unwrap_err();
        let joined = messages(errors).join("\n");
        assert!(joined.contains("schema `Server`"), "{joined}");
        assert!(joined.contains("has no matching struct in this file"), "{joined}");
        assert!(!joined.contains("did you mean"), "{joined}");
    }

    #[test]
    fn missing_struct_suggests_a_close_name() {
        let errors = schema_result("schema Server { host: str; };", "struct Servr { host: str = \"h\"; };")
            .unwrap_err();
        assert!(messages(errors).join("\n").contains("did you mean `Servr`?"));
    }

    #[test]
    fn distant_names_get_no_suggestion() {
        let errors = schema_result("schema Server { host: str; };", "struct Database { host: str = \"h\"; };")
            .unwrap_err();
        assert!(!messages(errors).join("\n").contains("did you mean"));
    }

    #[test]
    fn optional_schema_struct_may_be_omitted() {
        assert!(schema_result("schema? Cache { ttl: int; };", "struct Other { z: int = 1; };").is_ok());
    }

    #[test]
    fn private_section_is_validated_against_schema() {
        let errors = schema_result(
            "schema Server { host: str; port: int; };",
            "private struct Server { host: str = \"h\"; };",
        )
        .unwrap_err();
        assert!(messages(errors).iter().any(|m| m.contains("port")));
    }

    #[test]
    fn duplicate_schema_names_across_imports_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.spar"), "schema Server { host: str; };").unwrap();
        std::fs::write(dir.path().join("b.spar"), "schema Server { port: int; };").unwrap();
        let program = parse_src(
            "import schema \"a.spar\";\nimport schema \"b.spar\";\nstruct Server { host: str = \"h\"; port: int = 1; };\n",
        );
        let errors = validate_schema_imports(&program, dir.path()).unwrap_err();
        let joined = messages(errors).join("\n");
        assert!(joined.contains("schema `Server` is declared in both"), "{joined}");
        assert!(joined.contains("a.spar") && joined.contains("b.spar"), "{joined}");
    }

    #[test]
    fn all_schema_errors_are_reported_together() {
        let errors = schema_result(
            "schema A { x: int; };\nschema B { y: int; };",
            "struct Unrelated { z: int = 1; };",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 2);
    }
```
Run: `cd spar && cargo test --lib loader:: 2>&1 | grep -E "FAILED|test result"` → Expected: the new tests FAIL.

- [ ] **Step 2: Implement**

In `validate_schema_imports`:

1. Remove the `if s.private { continue; }` skip when building `config_sections` (private structs are matched).
2. Track which file first declared each schema name: `let mut schema_origin: HashMap<String, String> = HashMap::new();`. When building each import's `schema_sections`, before inserting name:

```rust
                if let Some(first) = schema_origin.get(&s.name) {
                    errors.push(SparError::SchemaError {
                        message: format!(
                            "schema `{}` is declared in both '{}' and '{}'",
                            s.name, first, decl.path
                        ),
                        span: decl.span.clone(),
                    });
                    continue;
                }
                schema_origin.insert(s.name.clone(), decl.path.clone());
```
(place inside the `for schema_item in &schema_prog.items` loop, wrapping the existing `schema_sections.insert(...)`).
3. Replace the "requires section `[X]`" message in Rule 1 with:

```rust
                None if !optional => {
                    let mut message = format!(
                        "schema `{}` ({}) has no matching struct in this file",
                        name, decl.path
                    );
                    if let Some(close) = closest_name(name, config_sections.keys().map(String::as_str)) {
                        message.push_str(&format!("; did you mean `{close}`?"));
                    }
                    errors.push(SparError::SchemaError {
                        message,
                        span: decl.span.clone(),
                    });
                }
```
4. Delete Rule 2 entirely (the `combined_schema_section_names` set, `has_schema_imports` flag, and the final loop).
5. Add the helper (edit distance ≤ 2, Levenshtein, deterministic tie-break by name):

```rust
fn closest_name<'a>(target: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        let distance = edit_distance(target, candidate);
        if distance > 2 {
            continue;
        }
        match best {
            Some((d, name)) if d < distance || (d == distance && name <= candidate) => {}
            _ => best = Some((distance, candidate)),
        }
    }
    best.map(|(_, name)| name)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}
```
Update the doc comment on `validate_schema_imports` ("verifies it has @SchemaFile" → "verifies it declares schemas"; "Rule 1/Rule 2" wording).

- [ ] **Step 3: Update the imported-section test**

`selectively_imported_section_is_not_checked_against_schema` (~2344) must use `schema Container { x?: str; };` and still pass: the schema names `Container` (present in config), the imported `Colors` has no schema and is ignored.

- [ ] **Step 4: Run tests**

Run: `cd spar && cargo test --lib loader:: 2>&1 | grep -E "FAILED|test result"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd spar && git add src/loader.rs
git commit -m "feat: match schemas to structs by name; unmatched structs are ignored" -- src/loader.rs
```

---

### Task 5: Migrate existing tests, examples, docs and fixtures

**Files:**
- Modify: every `.spar` source string/file that relies on implicit emit or old schema syntax: `spar/tests/*.rs`, `spar/src/tests/*.rs`, `spar/src/main.rs` tests (~1935-2020), `spar/tests/conformance/schema.spar` + `basic.spar` + others, `spar/examples/`, `examples/`, `spar/README.md`, `site/app/**` MDX pages, `spar/docs/*.md`, `spar-wasm` playground default source.

**Interfaces:** none new. Definition of done: `cargo test` green in spar, and grep for the old syntax returns nothing.

- [ ] **Step 1: Find failures**

Run: `cd spar && cargo test 2>&1 | grep -E "^test .*FAILED|test result" | sort | uniq`
Record the failing test names. Every failure should be one of: (a) emit output now empty/"nothing to emit", (b) old `@SchemaFile`/`Schema [`/`SchemaFrom [` syntax.

- [ ] **Step 2: Migrate emit-dependent sources**

For each failing test or fixture whose source is emitted (`spar emit`, `emit_to_*`, `build_emit_json`, `Engine::emit_*` that assert on JSON): add `#[emit]` on its own line above every top-level `struct`/`[Section]`/`export var` whose value the test asserts. `export var x` stays `export var` only where a cross-file import needs it; otherwise `#[emit]\nvar x` is preferred. Where a test emitted an `export var` only to see it in output, change to `#[emit]\nvar`.

- [ ] **Step 3: Migrate schema sources**

Mechanical rewrite for schema files and inline sources (run from repo root, review the diff before staging):

```bash
cd /home/occ/Projects/Rust/occ_lang
grep -rln "@SchemaFile\|Schema \[\|Schema? \[\|SchemaFrom" \
  spar/src spar/tests spar/README.md spar/docs spar/examples examples site/app spar-wasm/src 2>/dev/null
```
For each file: remove the `@SchemaFile` line; `Schema [Name]{` → `schema Name {`; `Schema? [Name]{` → `schema? Name {`; `SchemaFrom [A, B];` → `schema A from B;`; `SchemaFrom? [A, B];` → `schema? A from B;`. Also update tests that build Rust strings with `\n` escapes the same way. Also remove any assertion or doc sentence saying "section `[X]` is not declared in any imported schema" and replace docs with the matching rules from the spec.

- [ ] **Step 4: Docs and site**

In `spar/README.md`, `spar/docs/*.md` and `site/app/**/page.mdx`: every `spar emit` example needs `#[emit]` on the emitted declarations; the "visibility" explanation (README ~lines 104-135, `site/app/docs/language/export-variables/page.mdx`) must say: `#[emit]` controls output; `export` controls importing; `private` controls importing/spreading. Add a short "Emitting" section describing `#[emit]`, the nothing-to-emit error, and the schema matching table from the spec.

- [ ] **Step 5: Verify**

```bash
cd spar && cargo test 2>&1 | grep -E "test result|FAILED"
grep -rn "@SchemaFile\|Schema \[\|SchemaFrom \[" src tests README.md docs examples ../examples ../site/app ../spar-wasm/src 2>/dev/null | grep -v "was removed\|was replaced"
```
Expected: all tests PASS; the grep prints nothing (the removal-error tests intentionally contain the old text, hence the filter).

- [ ] **Step 6: Commit**

```bash
cd spar && git add -u src tests README.md docs examples
git commit -m "chore: migrate fixtures and docs to #[emit] and schema syntax" -- src tests README.md docs examples
```
(Site and root `examples/` live outside this repo's tracked tree if not tracked here; stage them in whichever repo tracks them: `git -C ../site status`.)

---

### Task 6: spar-ls support

**Files:**
- Modify: `spar-ls/src/completion.rs` (keyword list ~line 5, top-level completion), `spar-ls/src/semantic_tokens.rs` (~1275-1300 `collect_language_words`, `SchemaSection` arm ~1207), `spar-ls/src/hover.rs`, `spar-ls/src/main.rs` (test at ~3949 uses old syntax)
- Test: `spar-ls/src/main.rs` test module (existing helpers `decode_semantic_tokens`, `find_tok`)

**Interfaces:**
- Consumes: `spar::token::Token::HashBracket`, `spar::ast::KNOWN_ATTRIBUTES`.

- [ ] **Step 1: Failing tests**

In `spar-ls/src/main.rs` tests (next to `semantic_tokens_classify_decoder_namespace_name_and_option`) add:

```rust
    #[test]
    fn semantic_tokens_classify_attribute_and_schema_keyword() {
        let src = "#[emit]\nstruct A { x: int = 1; };\n";
        let tokens = decode_semantic_tokens(src);
        assert_eq!(
            find_tok(&tokens, "emit", src).unwrap().token_type,
            TT_DECLARATION_KEYWORD
        );
        let schema_src = "schema Server { host: str; };\n";
        let schema_tokens = decode_semantic_tokens(schema_src);
        assert_eq!(
            find_tok(&schema_tokens, "schema", schema_src).unwrap().token_type,
            TT_DECLARATION_KEYWORD
        );
    }
```
Also update the existing test at ~line 3949 (`"@SchemaFile\nSchema [Container]{ x?: str; };\n"`) to `"schema Container { x?: str; };\n"`. Add a completion test using the existing completion test helpers in `completion.rs`'s test module (find with `grep -n "fn .*test\|#\[test\]" spar-ls/src/completion.rs | head`): typing `#[` at line start yields a `emit` item:

```rust
    #[test]
    fn completes_attribute_names_after_hash_bracket() {
        let items = attribute_completion_items("#[");
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, vec!["emit"]);
    }
```
Run: `cd spar-ls && cargo test semantic_tokens_classify_attribute attribute_names 2>&1 | tail` → FAIL (function missing).

- [ ] **Step 2: Implement completion**

In `completion.rs` add:

```rust
/// Attribute-name completions offered after `#[`.
pub(crate) fn attribute_completion_items(line_prefix: &str) -> Vec<CompletionItem> {
    let Some(start) = line_prefix.rfind("#[") else {
        return Vec::new();
    };
    if line_prefix[start + 2..].contains(']') {
        return Vec::new();
    }
    spar::ast::KNOWN_ATTRIBUTES
        .iter()
        .map(|name| CompletionItem {
            label: (*name).to_string(),
            kind: Some(CompletionItemKind::PROPERTY),
            detail: Some("Emit this declaration in `spar emit` output".to_string()),
            ..Default::default()
        })
        .collect()
}
```
Wire it into the completion request handler: find where `keyword_items()` is used (`grep -n "keyword_items()" spar-ls/src/*.rs`) and, in the same function, before computing keyword items, compute the current line's text up to the cursor; if `attribute_completion_items(prefix)` is non-empty, return only those. Add `"schema"` and `"schema?"` are not needed as keywords; add `"schema"` to `keyword_items`'s declaration keywords list.

- [ ] **Step 3: Implement semantic tokens**

In `collect_language_words` change `SOFT_KEYWORDS` to `&["pkg", "from", "task", "type", "schema", "functionGroup"]` (drop `Schema`, `SchemaFrom`). Convert the loop to track attribute names:

```rust
    let mut after_hash_bracket = false;
    for spanned in &tokens {
        let (token_type, modifiers) = match &spanned.token {
            Token::HashBracket => {
                after_hash_bracket = true;
                (TT_DECLARATION_KEYWORD, MOD_NONE)
            }
            Token::Ident(word) if after_hash_bracket => {
                after_hash_bracket = false;
                let _ = word;
                (TT_DECLARATION_KEYWORD, MOD_NONE)
            }
            // ...existing arms unchanged...
```
and set `after_hash_bracket = false;` at the top of the `_ => continue` fallthrough is not needed because the next token after `#[` is always the identifier. Keep the existing arms after the two new ones.

- [ ] **Step 4: Hover**

In `hover.rs`, in the function that builds hover text for a word under the cursor (find with `grep -n "fn hover" spar-ls/src/hover.rs`), add before falling through: if the word is `emit` and the text immediately before it (on that line) ends with `#[`, return markdown:

```
**`#[emit]`**

Marks a top-level `struct` or `var` for output. `spar emit` writes only declarations that carry this attribute; `export` and `private` do not affect emission.
```
Add a unit test alongside existing hover tests asserting the returned markdown contains `#[emit]`.

- [ ] **Step 5: Run and commit**

Run: `cd spar-ls && cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy --all-targets -- -D warnings 2>&1 | tail -5`
Expected: PASS, Clippy clean.

```bash
cd spar-ls && git add src/completion.rs src/semantic_tokens.rs src/hover.rs src/main.rs
git commit -m "feat(ls): attribute completion, hover, and semantic tokens; schema keyword" -- src/completion.rs src/semantic_tokens.rs src/hover.rs src/main.rs
```

---

### Task 7: Tree-sitter and VS Code grammars

**Files:**
- Modify: `editors/tree-sitter-spar/grammar.js`, add `editors/tree-sitter-spar/test/corpus/attributes.txt` (and update any corpus file using old schema syntax: `grep -rln "Schema\|@SchemaFile" editors/tree-sitter-spar/test editors/tree-sitter-spar/queries`)
- Modify: `editors/vscode-spar/syntaxes/spar.tmLanguage.json`
- Each editor directory is its own git repo.

**Interfaces:** none.

- [ ] **Step 1: tree-sitter corpus test (failing)**

Create `editors/tree-sitter-spar/test/corpus/attributes.txt`:

```
==================
emit attribute on struct
==================

#[emit]
struct Server { port: int = 1; };

---

(source_file
  (attribute (identifier))
  (struct_declaration))

==================
schema declaration
==================

schema Server { host: str; };

---

(source_file
  (schema_declaration))
```
Run: `cd editors/tree-sitter-spar && npx tree-sitter generate && npx tree-sitter test 2>&1 | tail -20` → Expected: FAIL. Before editing, view how the existing grammar names its rules for struct/var (`grep -n "struct\|_declaration" grammar.js | head -30`) and use those exact node names in the corpus expectations.

- [ ] **Step 2: Grammar change**

In `grammar.js` add an `attribute` rule and allow zero or more before top-level `var`/`struct`/section rules, and replace the old `Schema [..]`/`SchemaFrom [..]` rules:

```js
    attribute: $ => seq('#[', $.identifier, ']'),
    schema_declaration: $ => seq(
      'schema', optional('?'), $.identifier,
      choice(
        seq('{', repeat($.schema_field), '}', ';'),
        seq('from', $.identifier, ';'),
      ),
    ),
```
Wrap the existing top-level declaration alternatives for variable/struct/section as `seq(repeat($.attribute), $._existing)`; remove the `@SchemaFile` pragma alternative if present. Run `npx tree-sitter generate && npx tree-sitter test` → PASS (update other corpus entries that used old syntax).

- [ ] **Step 3: TextMate grammar**

In `spar.tmLanguage.json` add a pattern object (in `patterns` at top level and wherever declarations are listed):

```json
{
  "name": "meta.attribute.spar",
  "begin": "#\\[",
  "beginCaptures": { "0": { "name": "punctuation.definition.attribute.begin.spar" } },
  "end": "\\]",
  "endCaptures": { "0": { "name": "punctuation.definition.attribute.end.spar" } },
  "patterns": [{ "name": "entity.other.attribute-name.spar", "match": "[A-Za-z_][A-Za-z0-9_]*" }]
}
```
and change the schema keyword pattern from `\\b(Schema|SchemaFrom)\\b` to `\\bschema\\b\\??` scoped `keyword.declaration.spar`. Validate the JSON: `python3 -c "import json;json.load(open('editors/vscode-spar/syntaxes/spar.tmLanguage.json'))"`.

- [ ] **Step 4: Commit in each editor repo**

```bash
cd editors/tree-sitter-spar && git add grammar.js test/corpus && git commit -m "feat: attributes and schema declaration syntax" -- grammar.js test/corpus
cd ../vscode-spar && git add syntaxes/spar.tmLanguage.json && git commit -m "feat: highlight attributes and schema declarations" -- syntaxes/spar.tmLanguage.json
```
(Add generated parser files to the tree-sitter commit only if the repo tracks `src/`: `git -C editors/tree-sitter-spar ls-files src | head -1`.)

---

### Task 8: Version bump, wasm, install, full verification

**Files:**
- Modify: `spar/Cargo.toml` (`version = "0.6.0"`), `spar-ls/Cargo.toml` (`version = "0.6.0"` and its `spar` dependency pin), lock files as regenerated
- Regenerate: `site/vendor/spar-wasm/*`

- [ ] **Step 1: Bump versions**

Set `version = "0.6.0"` in `spar/Cargo.toml` and `spar-ls/Cargo.toml`; update spar-ls's dependency on spar to `0.6.0` (path dep with `version = "0.6.0"` if it pins one). `spar-wasm` depends on spar by path; leave its version.

- [ ] **Step 2: Full verification**

```bash
cd spar && cargo fmt --all && cargo test --all-targets --all-features 2>&1 | grep -E "test result|FAILED" && cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
cd ../spar-ls && cargo fmt --all && cargo test --all-targets 2>&1 | grep -E "test result|FAILED" && cargo clippy --all-targets -- -D warnings 2>&1 | tail -3
cd ../sparsh && cargo test --workspace 2>&1 | grep -E "test result|FAILED"
```
Expected: all PASS, Clippy clean. If sparsh tests fail because config or test sources emit or use old schema syntax, fix those sources with `#[emit]`/new syntax in the same way as Task 5.

- [ ] **Step 3: Rebuild wasm**

Per `CLAUDE.md`:
```bash
cd spar-wasm && wasm-pack build --target web --release --out-dir ../site/vendor/spar-wasm && rm -f ../site/vendor/spar-wasm/.gitignore
```
Update the playground's default sample source under `site/` (search `grep -rn "export var" site/app site/components 2>/dev/null | head`) to use `#[emit]`.

- [ ] **Step 4: Reinstall binaries (required after every spar commit)**

```bash
cargo install --path spar --force && cargo install --path spar-ls --force
```
Smoke check:
```bash
cd /tmp && printf '#[emit]\nstruct A { x: int = 1; };\nvar hidden: int = 2;\n' > emit_smoke.spar && spar emit emit_smoke.spar
```
Expected output: `{ "A": { "x": 1 } }` (pretty-printed) and no `hidden`.

- [ ] **Step 5: Commit**

```bash
cd spar && git add Cargo.toml Cargo.lock && git commit -m "chore: bump spar to 0.6.0" -- Cargo.toml Cargo.lock
cd ../spar-ls && git add Cargo.toml Cargo.lock && git commit -m "chore: bump spar-ls to 0.6.0" -- Cargo.toml Cargo.lock
```

---

## Self-review notes

- Spec coverage: attributes (T1), emit semantics + nothing-to-emit + serde untouched + sorted keys (T2, Global Constraints), schema syntax + removal errors (T3), matching table incl. duplicates/typo hint/private/optional/all-errors (T4), migration (T5), tooling (T1 formatter, T6 ls, T7 grammars), rollout/wasm/version/reinstall (T8).
- Deviation from spec: unknown-attribute validation happens in the parser when attributes attach (not the resolver). Users see the same error with a span; noted here so the spec's "resolver validates" line is read as "at attach time".
- Type consistency: `Attribute`, `KNOWN_ATTRIBUTES`, `is_emit`, `HashBracket`, `emit` symbol flag names are identical across tasks.
