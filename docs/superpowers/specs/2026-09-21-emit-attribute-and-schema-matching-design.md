# `#[emit]` attribute and schema matching by struct name

Date: 2026-09-21
Status: approved design, not implemented

## Goal

Spar files are now programs, not just config. Today every top-level struct is emitted unless
marked `private`, and `export var` puts values at the JSON root. A file that reads secrets with
`env()` can therefore dump them by accident when someone runs `spar emit`. This design makes
emission explicit and predictable:

1. Nothing is emitted unless a top-level declaration carries the `#[emit]` attribute.
2. Schemas match structs by name, in a new `schema Name { ... };` syntax that replaces the old
   bracket form.

This is a breaking language change (spar 0.6.0). There are no other users, so there is no
compatibility mode; existing files, tests, docs and examples are migrated in the same change.

## Attributes

### Syntax

```spar
#[emit]
struct Server { host: str = "0.0.0.0"; port: int = 443; };

#[emit]
var version: str = "1.4.2";
```

- The lexer gets a `#[` token. (`#{` interpolation and `#` handling elsewhere are unchanged.)
- The parser collects one or more `#[Name]` (later: `#[Name(args)]`) entries preceding a top-level
  declaration and stores them as `attributes: Vec<Attribute>` on that declaration. Stacking
  (`#[emit] #[other]`) is allowed.
- Valid targets: top-level `struct`, `[Section]`, and `var`. Any other placement (functions,
  fields, dangling at end of file) is an error:
  ``attribute `#[emit]` is only valid on top-level structs and vars``.
- The resolver validates attribute names. The only known name is `emit`. An unknown attribute is an
  error listing valid names. This makes future native attributes a resolver addition, not a grammar
  change.

### Emit semantics

- `spar emit`, `emit_to_json` / `emit_to_yaml` / `emit_to_toml`, and the wasm playground output only
  declarations marked `#[emit]`.
- `export` now means only "importable from other files". `private` likewise only affects
  importing/spreading. Neither affects emit.
- Nested structs inside an emitted struct are emitted as part of it. There is no per-field control:
  everything inside an `#[emit]` struct is emitted, and the attribute sits visibly at the declaration.
- Output key order is unchanged: keys are sorted alphabetically at every level.
- If a file has no `#[emit]` items, `spar emit` exits non-zero with
  `nothing to emit: mark top-level structs or vars with #[emit]`. It never silently prints `{}`.
- The serde API (`spar::from_str`, `spar::from_eval`) is unaffected: it reads structs and vars
  regardless of `#[emit]`, because it only reads the fields the Rust struct names.

## Schemas

### Syntax (replaces `@SchemaFile` and `Schema [Name]{}`)

```spar
// schema/server.spar
schema Server {
    host: str;
    port: int;
    ssl?: bool;
};

schema? Cache {          // optional: the importing file may omit it
    ttl: int;
};

schema Db from DbType;   // was: SchemaFrom [Db, DbType];
```

- A file containing `schema` declarations is a schema file; `@SchemaFile` is no longer used. A schema
  file may contain only `schema` declarations and `import type { ... } from "...";`.
- Old forms produce targeted errors:
  ``@SchemaFile was removed; declare `schema Name { ... };` `` and
  ``Schema [Name]{...} was replaced by `schema Name { ... };` ``.

### Matching rules

`import schema "schema/server.spar";` matches schema names against the importing file's top-level
struct names. Matching is exact and case-sensitive.

| Case | Result |
|---|---|
| Schema `Server` and struct `Server` both exist | Matched. The struct is type-checked: missing required fields, undeclared fields, type mismatches are errors (unchanged checks). |
| Struct exists, no schema has that name | Ignored: a plain struct, no schema error. This replaces the old "Rule 2" error. |
| Required `schema Server`, no struct `Server` | Error: ``schema `Server` (schema/server.spar:3) has no matching struct in this file``, with ``did you mean `Servr`?`` when a struct name is within edit distance 2. |
| `schema? Cache`, no struct `Cache` | Fine. |
| Two imported schema files declare the same name | Error naming both files. Schema names share one namespace per importing file. |

- Matching covers all top-level structs, including `private` ones. Validation is independent of
  visibility and of `#[emit]`.
- Schemas never cause emission: a matched struct still needs `#[emit]`.
- Schemas describe struct fields only; there are no schema declarations for top-level vars.
- Errors use the existing renderer and `error[schema]` category with source spans, and are all
  collected and reported together rather than stopping at the first.

## Tooling

- **Formatter:** keeps attributes attached to their declaration, one per line above it; idempotent.
  Formats the new `schema` syntax.
- **spar-ls:** completion for attribute names after `#[`, hover text for `emit`, semantic tokens for
  attributes and the `schema` keyword; diagnostics carry the new errors.
- **tree-sitter grammar and VS Code TextMate grammar:** add `#[...]`, `schema`, `schema?`, and
  `schema Name from Type`.
- **wasm/playground:** rebuild after the compiler change so the site matches.

## Testing

1. **Lexer/parser:** `#[emit]` attaches to the next top-level struct/section/var; stacking parses;
   attributes on functions, fields, or nothing are rejected with the placement error; unknown attribute
   rejected with the valid-names list; `schema`, `schema?`, `schema Name from Type;` parse;
   `@SchemaFile` and `Schema [X]{}` give the removal errors.
2. **Emit** (unit tests plus CLI golden tests for JSON, YAML, TOML): only `#[emit]` items appear;
   `export var` without `#[emit]` does not emit (regression test for the `env()` leak); `private`
   does not affect emit; nothing-to-emit exits non-zero with the message; nested structs come along;
   `emit_to_*` and wasm follow the same rules; serde `from_str`/`from_eval` still read
   non-`#[emit]` values.
3. **Schema matching:** one test per row of the matching table; typo hint fires only within edit
   distance 2; duplicate name across two imported files errors; private structs are matched; all
   schema errors in a file are reported together; the matched-struct type checks are unchanged.
4. **Tooling:** formatter round-trip keeps attributes attached and is idempotent; spar-ls tests for
   completion after `#[`, hover, and semantic tokens; tree-sitter corpus and TextMate grammar updates.

## Rollout

Breaking change: `spar` and `spar-ls` go to 0.6.0 together.

1. **Language core (spar crate):** lexer, parser, AST, resolver validation, emit gating, schema
   matching, new error messages.
2. **Mass migration in the same change:** add `#[emit]` to every existing test, example, README snippet,
   and site doc that relies on implicit emit; convert schema files and tests to the new syntax. The full
   suite is the check that nothing was missed.
3. **Tooling:** formatter, then spar-ls, then tree-sitter and the VS Code grammar.
4. **Distribution:** rebuild wasm, update site docs and README, reinstall `spar` and `spar-ls`.

Independent of the sparsh config package work (see
`2026-09-21-sparsh-config-package-design.md`); B lands first so the fixture migration happens while
spar is a single tree.

## Out of scope

Per-field emit control, attributes with arguments (the parser shape allows them later), schemas for
top-level vars, and any compatibility mode for the old schema syntax.
