# SCOC Master Design

**Date:** 2026-09-21  
**Project:** SCOC — SPA Command Output Converter  
**Status:** Written design awaiting user review  
**Implementation status:** Not started by this design  
**Initial compatibility baseline:** `jc` 1.26.0 at commit `73fa7d5572dd730076723bd6280786bb9101d32f`

## 1. Purpose

SCOC is a Rust-native command-output conversion library for the Spar/Sparsh ecosystem. Its purpose is to turn unstructured command or file output into structured data that Spar can process directly.

The user-facing goal is:

```spar
df | from df
ps aux | from ps
ls -l | from ls
ping 1.1.1.1 | from ping
cat /etc/fstab | from fstab
```

The resulting values immediately participate in Spar structured pipelines:

```spar
df
| from df
|> where(fn(row) => row.use_percent > 80)
|> select(["filesystem", "mounted_on", "use_percent"])
```

SCOC is inspired by and behaviorally compatible with Kelly Brazil's Python `jc` project where practical, but it is not a Python embedding and does not reproduce JC's internal architecture. SCOC ports parser behavior into native Rust and integrates with Spar's existing `from` bridge.

The success criteria are:

1. `from <parser>` converts command output directly into Spar structured values without a JSON text round-trip.
2. SCOC parsers preserve JC field names, types, null behavior, normalization, and parser semantics by default unless an explicit deviation is documented.
3. Live inputs automatically use streaming parsers when the selected parser supports streaming.
4. Native Spar codecs keep their existing behavior and precedence.
5. SCOC is a standalone Rust library with no dependency on Spar, Sparsh, `spar-process`, or `spar-ls`.
6. Parser parity is demonstrated with both native Rust fixture tests and differential tests against a pinned JC baseline.
7. Linux and macOS are the first-class initial platform targets. Windows is deferred to a later parity phase.
8. Users can eventually register custom parsers in Spar without native dynamic plugins or a second package manager.

## 2. Existing Spar integration points

The current Spar source already has the byte-to-value and value-to-byte mixed-pipeline architecture that SCOC should extend rather than replace.

At the time of this design:

- `spar/src/ast.rs` defines `ShellCodecStage` and `ShellMixedPipeline`.
- `ShellMixedPipeline` owns an input command pipeline, a decoder stage, structured `|>` stages, an optional encoder stage, and optional downstream byte commands/redirection.
- `spar/src/shell_lang.rs` recognizes `from FORMAT` as the byte-to-value bridge and currently requires exactly one literal format name.
- `spar/src/structured_codec.rs` defines `StructuredFormatRegistry`, built-in codecs, `StructuredParser`, and `StructuredSerializer`.
- `StructuredFormatRegistry` currently contains the native formats `json`, `jsonl`, `csv`, `tsv`, `lines`, `yaml`, `toml`, and `text` plus aliases.
- `spar/src/runtime.rs` creates `StructuredFormatRegistry::builtin()` when executing a mixed pipeline, obtains the decoder parser and serializer, streams process output, and wraps decoded values in Spar's structured stream resource.
- The existing decoder implementation already distinguishes buffered and incremental codec states. SCOC must preserve the same lazy mixed-pipeline behavior rather than forcing all inputs into a complete in-memory buffer.

SCOC therefore extends the input-decoder side of the existing bridge. It does not redefine `|`, `|>`, or `to`.

## 3. Product scope

### 3.1 Spar-native integration

SCOC is primarily a library embedded by Spar. A standalone SCOC CLI is not a version-one requirement.

Canonical usage is:

```spar
df | from df
```

not:

```text
df | scoc --df
```

### 3.2 Existing native codecs remain native

Existing Spar format decoding continues to work:

```spar
cat users.json | from json
cat users.yaml | from yaml
cat users.csv  | from csv
```

When a name exists in both native Spar codecs and SCOC, the native codec wins for the unqualified name:

```spar
cat users.csv | from csv        // codec::csv
cat users.csv | from scoc::csv  // JC-compatible SCOC parser
```

### 3.3 Compatibility policy

SCOC is JC-compatible by default, with Spar-native extensions layered above that contract.

Compatibility means observable parser behavior, not source-code similarity. Default SCOC output preserves compatible JC:

- field names;
- field types;
- null behavior;
- list/object shape;
- normalization;
- raw-mode behavior;
- record ordering where observable;
- streaming record boundaries;
- parser error behavior when it is part of the parser contract.

SCOC must not automatically rename JC-compatible fields to Spar-style camelCase. For example, if JC returns `use_percent` and `mounted_on`, SCOC returns those same keys.

### 3.4 Platforms

The initial supported platform target is:

- Linux;
- macOS.

Windows parser parity is deferred to a later project. The type system may model Windows, FreeBSD, and other upstream platforms from the beginning, but a parser must not be advertised as SCOC-supported on an unverified platform merely because upstream JC lists that platform.

## 4. Repository and dependency boundary

SCOC is a separate Rust repository/crate alongside the existing projects:

```text
occ_lang/
├── spar/
├── spar-command/
├── spar-process/
├── sparsh/
├── spar-ls/
└── scoc/
```

The dependency direction is one-way:

```text
scoc
  ↑
  │
spar
  ↑
  ├── sparsh
  └── spar-ls integration metadata path
```

SCOC must not depend on:

- `spar`;
- `sparsh`;
- `spar-process`;
- `spar-ls`;
- Reedline;
- a terminal renderer;
- Python or JC at runtime.

SCOC's job is only to parse bytes or line streams into structured Rust values.

## 5. SCOC crate architecture

SCOC begins as one Rust crate, internally separated into focused modules:

```text
scoc/
├── Cargo.toml
├── THIRD_PARTY_LICENSES/
│   └── JC-MIT.txt
├── src/
│   ├── lib.rs
│   ├── parser.rs
│   ├── registry.rs
│   ├── descriptor.rs
│   ├── options.rs
│   ├── error.rs
│   ├── stream.rs
│   ├── utils/
│   │   ├── mod.rs
│   │   ├── numbers.rs
│   │   ├── sizes.rs
│   │   ├── datetime.rs
│   │   ├── tables.rs
│   │   ├── key_value.rs
│   │   ├── text.rs
│   │   └── platform.rs
│   └── parsers/
│       ├── mod.rs
│       ├── df.rs
│       ├── ps.rs
│       ├── ls.rs
│       ├── ping.rs
│       └── ...
├── tests/
│   ├── parsers/
│   ├── fixtures/
│   └── compatibility/
└── compatibility/
    ├── jc-baseline.toml
    └── parser-matrix.toml
```

The crate should not be split into `scoc-core`, `scoc-parsers`, and other subcrates unless actual compile-time, ownership, or feature-management pressure later justifies that split.

## 6. Structured value representation

SCOC's public structured representation is `serde_json::Value`.

This does not mean SCOC serializes data to JSON text. It means JC-compatible JSON-shaped data is represented as an in-memory Rust enum.

SCOC should enable `serde_json`'s `preserve_order` feature so object insertion order remains stable where practical. The exact dependency version is chosen by the implementation plan and normal dependency policy, not by this architecture spec.

The Spar adapter recursively converts in memory:

```text
serde_json::Value::Null   → Spar null
Bool                      → Spar bool
Number                    → Spar int/float
String                    → Spar str
Array                     → Spar List/Table according to descriptor
Object                    → Spar Record
```

The bridge must never perform this wasteful path:

```text
serde_json::Value
    ↓ serialize
JSON text
    ↓ parse
Spar Value
```

## 7. Core parser contract

SCOC uses a parser trait plus registry architecture. The exact Rust signatures may evolve during implementation, but the semantic contract is:

```rust
pub trait ScocParser: Send + Sync {
    fn descriptor(&self) -> &'static ParserDescriptor;

    fn parse(
        &self,
        input: &[u8],
        options: &ParseOptions,
    ) -> Result<serde_json::Value, ScocError>;

    fn stream_parser(
        &self,
        options: &ParseOptions,
    ) -> Result<Box<dyn ScocStreamParser>, ScocError> {
        Err(ScocError::StreamingUnsupported {
            parser: self.descriptor().name,
        })
    }
}

pub trait ScocStreamParser: Send {
    fn push(
        &mut self,
        chunk: &[u8],
    ) -> Result<Vec<serde_json::Value>, ScocError>;

    fn finish(
        self: Box<Self>,
    ) -> Result<Vec<serde_json::Value>, ScocError>;
}
```

The streaming contract is deliberately incremental and push-based. This matches Spar's existing mixed-pipeline runtime, which already pulls stdout chunks from `spar-process`, feeds them into a decoder with `push`, and calls `finish` at EOF. SCOC therefore does not own a blocking reader, process thread, or async runtime. `spar-process` remains responsible for subprocess I/O, process lifetime, cancellation, and backpressure; Spar feeds stdout chunks into the SCOC stream parser.

## 8. Parser descriptors

Each built-in parser publishes typed metadata.

Conceptually:

```rust
pub struct ParserDescriptor {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub parser_version: &'static str,
    pub platforms: &'static [Platform],
    pub tags: &'static [ParserTag],
    pub output: ParserOutput,
    pub capabilities: ParserCapabilities,
    pub options: &'static [OptionSpec],
    pub upstream: Option<UpstreamParser>,
}
```

Capabilities are grouped:

```rust
pub struct ParserCapabilities {
    pub raw: bool,
    pub streaming: bool,
    pub ignore_errors: bool,
}
```

Output shape is explicit rather than guessed from JSON structure. Batch output and streaming item output are tracked separately because a parser can return a collection in batch mode while emitting one record at a time in streaming mode:

```rust
pub struct ParserOutput {
    pub batch: OutputShape,
    pub stream_item: Option<OutputShape>,
}

pub enum OutputShape {
    Scalar,
    Record,
    List,
    Table,
}
```

The upstream mapping tracks the JC relationship without duplicating the global JC release baseline:

```rust
pub struct UpstreamParser {
    pub standard_name: &'static str,
    pub streaming_name: Option<&'static str>,
    pub parser_version: Option<&'static str>,
}
```

This metadata drives runtime dispatch, compatibility reports, documentation, and `spar-ls` completion/hover/diagnostics.

## 9. Parser options

Parser options use a typed schema rather than an unvalidated `HashMap<String, Value>`.

Conceptually:

```rust
pub enum OptionKind {
    Bool,
    Integer,
    Float,
    String,
    Enum(&'static [&'static str]),
}

pub struct OptionSpec {
    pub name: &'static str,
    pub kind: OptionKind,
    pub required: bool,
    pub default: Option<OptionValue>,
    pub description: &'static str,
}
```

Spar syntax uses named arguments:

```spar
df | from df(raw: true)
ping 1.1.1.1 | from ping(ignoreErrors: true)
```

The user does not inherit JC CLI flags such as `-r` or `-qq` into Spar syntax.

Parser arguments are ordinary Spar expressions where allowed:

```spar
var useRaw = true;
df | from df(raw: useRaw)
```

The LSP/compiler should reject unknown options and obvious type errors before runtime when descriptor information is available.

`streaming` is different from parser options. It controls how Spar feeds SCOC, so it is owned by the Spar decoder layer rather than `ParseOptions`:

```spar
ping ... | from ping(streaming: false)
```

`raw` and `ignoreErrors` are capabilities. They are accepted only for parsers that explicitly advertise them.

JC CLI/API presentation controls that do not materially change parser data are not copied automatically. SCOC exposes only behavior needed for its native parsing contract and compatibility target.

## 10. Raw and normalized output

For parsers with raw-mode support, SCOC should conceptually separate structural parsing from normalization:

```text
raw command output
        ↓
structural parse
        ↓
raw structured representation
        ↓
normalization
        ↓
JC-compatible normalized schema
```

Normal use performs both stages:

```spar
df | from df
```

Raw mode stops after structural parsing:

```spar
df | from df(raw: true)
```

Compatibility tests must independently verify raw and normalized behavior where upstream JC supports raw mode.

## 11. Error model

SCOC returns typed errors, conceptually including:

```rust
pub enum ScocError {
    UnknownParser { name: String },
    UnsupportedPlatform { parser: String, platform: Platform },
    InvalidOption { parser: String, option: String, reason: String },
    InvalidInput { parser: String, line: Option<usize>, message: String },
    StreamingUnsupported { parser: &'static str },
    Parse {
        parser: String,
        line: Option<usize>,
        input: Option<String>,
        message: String,
    },
}
```

Spar translates these into normal Spar diagnostics rather than exposing Rust debug errors.

Batch parsing is strict by default. Invalid input should fail the pipeline rather than silently return an empty table unless the pinned compatibility contract explicitly requires a different result.

Streaming parsing is also strict by default. For JC-compatible streaming parsers that support ignore-exception behavior:

```spar
ping ... | from ping(ignoreErrors: true)
```

SCOC preserves the compatible per-record behavior, including `_jc_meta` where that is part of JC's observable output.

Custom Spar parsers do not automatically receive `_jc_meta`; they control their own recoverable-error schema.

## 12. Shared parser utilities

SCOC should extract shared Rust utilities only when real parser ports demonstrate reuse. Likely shared utilities include:

- sparse and fixed-width tables;
- key/value formats;
- integer and float normalization;
- percentages;
- human-readable sizes;
- dates/timestamps;
- whitespace normalization;
- IP/network value parsing;
- repeated keys and multi-value fields.

The project must not mechanically port every JC utility function before any parser needs it. Compatibility is measured at parser outputs, not by reproducing JC's Python internals.

## 13. Public SCOC API

The public Rust surface should remain small even when the parser inventory grows substantially.

Conceptually:

```rust
pub fn parse(
    parser: &str,
    input: &[u8],
    options: &ParseOptions,
) -> Result<serde_json::Value, ScocError>;

pub fn stream_parser(
    parser: &str,
    options: &ParseOptions,
) -> Result<Box<dyn ScocStreamParser>, ScocError>;

pub fn parser(name: &str) -> Option<&'static ParserDescriptor>;
pub fn parsers() -> impl Iterator<Item = &'static ParserDescriptor>;
pub fn registry() -> &'static ParserRegistry;
pub fn compatibility_baseline() -> &'static CompatibilityBaseline;
```

The built-in registry is immutable for ordinary callers. User-defined Spar parsers are registered in Spar's higher-level decoder registry, not inside SCOC.

## 14. Parser naming

There are three distinct naming systems and none should be distorted to satisfy another.

Rust modules use Rust naming:

```text
git_log.rs
apt_cache_show.rs
ip_route.rs
```

Canonical SCOC parser names preserve the shell/upstream form:

```text
git-log
apt-cache-show
ip-route
```

Spar custom parser declarations use normal Spar declaration naming rules and expose a separate shell-facing name field:

```spar
parser GitLogCustom {
    description: "Custom Git log parser";
    name: "git-log-custom";
    output: table;
    // ...
}
```

The declaration identifier `GitLogCustom` is a normal Spar symbol. The `name` value is a required compile-time string literal used for decoder registration.

## 15. Streaming model

### 15.1 Automatic streaming

Automatic streaming is owned by Spar, not SCOC.

SCOC exposes:

```text
parse()
stream_parser() → incremental push/finish parser
supports_streaming metadata
```

Spar decides whether to batch or incrementally decode based on input liveness, parser capability, and the `streaming` override.

The decoder-level policy is tri-state:

```rust
pub enum StreamingMode {
    Auto,
    Enabled,
    Disabled,
}
```

The behavior is:

| Input | Parser supports streaming | Setting | Behavior |
|---|---:|---|---|
| live | yes | auto | stream |
| live | no | auto | buffer then batch-parse |
| buffered | yes | auto | batch-parse |
| buffered | no | auto | batch-parse |
| live/buffered | yes | true | use streaming parser |
| any | no | true | diagnostic |
| any | any | false | buffer then batch-parse |

Canonical usage therefore stays simple:

```spar
ping 1.1.1.1 | from ping
```

while explicit overrides remain available:

```spar
ping 1.1.1.1 | from ping(streaming: true)
ping 1.1.1.1 | from ping(streaming: false)
```

### 15.2 Streaming aliases

For upstream JC pairs such as `ping` / `ping-s`, `git-log` / `git-log-s`, or `top` / `top-s`, the canonical SCOC parser name is the non-`-s` name because streaming selection is automatic.

The SCOC descriptor records the upstream streaming name, but the special `-s` behavior is exposed by Spar's `scoc::` decoder namespace rather than by overloading SCOC's batch `parse()` API:

```spar
from scoc::ping-s
from scoc::git-log-s
```

At the Spar decoder layer, those compatibility names resolve to the canonical SCOC parser plus `StreamingMode::Enabled`. `spar-ls` labels them as JC compatibility aliases and presents the non-`-s` form as canonical. This keeps SCOC's Rust API unambiguous while retaining an obvious migration path for JC users.

### 15.3 Cancellation and backpressure

A live streaming path is:

```text
spar-process
     ↓ bytes/lines
SCOC streaming parser
     ↓ serde_json::Value items
Spar value adapter
     ↓ Stream<Record>/Stream<Value>
|> operators
```

The stream must remain lazy. A pipeline such as:

```spar
ping 1.1.1.1
| from ping
|> take(5)
```

must be able to stop consuming after five required records. Cancellation must propagate through the Spar stream to `spar-process` as appropriate. SCOC itself does not own process termination.

## 16. Spar decoder architecture

### 16.1 Generalized input registry

The existing `StructuredFormatRegistry` remains valuable for native codecs, but `from` needs a higher-level registry that can resolve three decoder classes:

```text
StructuredInputRegistry
├── codec::
│   ├── json
│   ├── jsonl
│   ├── csv
│   ├── tsv
│   ├── yaml
│   ├── toml
│   ├── lines
│   └── text
├── scoc::
│   ├── df
│   ├── ps
│   ├── ls
│   ├── ping
│   └── ...
└── custom::
    └── user-defined Spar parsers
```

A conceptual internal enum is:

```rust
pub enum StructuredDecoder {
    Codec(CodecDecoder),
    Scoc(ScocDecoder),
    Custom(CustomDecoder),
}
```

The higher-level decoder descriptor used by Spar and `spar-ls` should normalize metadata from codecs, SCOC, and custom parsers into one interface.

### 16.2 Resolution precedence

For an unqualified decoder name, resolution order is:

1. native Spar codec;
2. SCOC parser;
3. custom Spar parser;
4. otherwise an unknown-decoder diagnostic.

Therefore:

```spar
from csv
```

means `codec::csv`, while:

```spar
from df
```

means `scoc::df`.

Explicit namespaces always bypass precedence:

```spar
from codec::csv
from scoc::csv
from scoc::df
from custom::df
```

A custom parser may reuse a built-in name, but it does not silently override that built-in for the unqualified form.

## 17. `from` grammar and AST evolution

The current bridge accepts exactly one literal format name. SCOC requires a richer decoder stage:

```text
from <decoder-ref> [(named-arguments)]
```

Examples:

```spar
df | from df
df | from df(raw: true)
git log | from git-log
cat data.csv | from codec::csv
ping 1.1.1.1 | from scoc::ping(streaming: false)
```

Decoder references need a context-specific grammar so hyphenated names are not parsed as subtraction expressions:

```text
DecoderRef := DecoderName | Namespace "::" DecoderName
DecoderName := [A-Za-z][A-Za-z0-9_-]*
Namespace := codec | scoc | custom
```

This special name grammar applies only to decoder references; it does not change normal Spar identifier rules.

The current shared `ShellCodecStage` should evolve so decode and encode stages no longer pretend to be the same abstraction. Conceptually:

```rust
pub struct ShellDecodeStage {
    pub decoder: DecoderRef,
    pub args: Vec<NamedDecoderArg>,
    pub span: Span,
}

pub struct ShellEncodeStage {
    pub format: String,
    pub span: Span,
}
```

The exact compiled representation should mirror the same separation.

`to FORMAT` remains serialization-oriented. SCOC does not create meaningless forms such as `to df`.

## 18. Runtime data flow

A batch SCOC decoder path is:

```text
external command
     ↓ stdout bytes
spar-process
     ↓
Spar decoder resolution
     ↓
scoc::parse(...)
     ↓
serde_json::Value
     ↓
Spar Value / Record / Table
     ↓
|> stages
     ↓
Sparsh renderer or `to` serializer
```

A live streaming path is:

```text
external command
     ↓ live stdout
spar-process
     ↓
Spar obtains SCOC stream parser
     ↓ push(stdout chunks) / finish()
SCOC records
     ↓
Spar Stream<Value>
     ↓
|> stages
```

Spar should preserve the existing mixed-pipeline lazy structure. It must not buffer solely because the decoder is SCOC when the parser can stream.

## 19. Custom Spar parsers

### 19.1 Ownership

Custom parsers are a Spar language/runtime feature, not SCOC plugins. SCOC remains unaware of them.

Built-ins use Rust/SCOC. Native formats use Spar codecs. User extensions use Spar parser declarations. All three participate in the higher-level `StructuredInputRegistry`.

### 19.2 Declaration shape

The approved language direction uses a normal Spar declaration identifier plus an explicit shell-facing `name` field:

```spar
parser CompanyStatus {
    description: "Parse output from our internal status command";
    name: "company-status";
    output: table;

    options {
        verbose: bool = false;
    }

    function parse(input, options) {
        // return a structured Spar value
    }
}
```

The declaration identifier follows the same naming conventions as other Spar declarations. SCOC/Spar must not introduce a special form such as:

```spar
parser "company-status" {}
```

`name` is a required compile-time string literal so the compiler and `spar-ls` can know the registration name statically.

### 19.3 Registration and collisions

A custom parser becomes active when the Spar module containing its declaration is loaded/imported. There is no automatic scanning of arbitrary plugin directories.

Duplicate custom registration of the same shell-facing name is an error. A custom parser may share a name with a codec or SCOC parser because namespaces distinguish them:

```spar
from df          // built-in SCOC df
from custom::df  // user parser named df
```

### 19.4 Streaming custom parsers

The custom-parser design must reserve the same logical capabilities as SCOC:

- parse function;
- optional stream function;
- output shape;
- option schema;
- streaming capability.

If a custom parser provides a stream implementation, automatic streaming can select it for live sources. If it provides only a parse function, it is a buffered decoder.

The exact custom-parser grammar, resolver integration, module behavior, and runtime function signatures are intentionally a separate architectural spec before implementation. This master design fixes the declaration naming model and registry contract but does not authorize implementing the full custom-parser declaration in the first SCOC milestone.

### 19.5 Security model

Custom parsers are ordinary Spar code and receive no special privilege or sandbox claim. Loading an untrusted parser module has the same trust implications as loading any other untrusted Spar module.

There is no native `.so` plugin ABI, no Python plugin system, and no automatic plugin execution directory.

When Spar's package manager is ready, custom parser libraries should be ordinary Spar packages. SCOC does not get its own package manager.

## 20. `spar-ls` integration

`spar-ls` should obtain decoder metadata from the same conceptual registry model rather than maintain a manually duplicated parser list.

Required editor behaviors include:

- completion after `from`;
- namespace completion for `codec::`, `scoc::`, and `custom::`;
- parser-specific named-argument completion;
- hover documentation;
- parser output-shape information;
- platform support information;
- compatibility alias labeling;
- diagnostics for unknown decoders, unknown namespaces, unsupported platforms, unknown options, wrong option types, duplicate named arguments, and impossible streaming requests;
- semantic highlighting that distinguishes external commands, `from`, decoder names, named options, and values.

Example hover information:

```text
df
SCOC command-output parser

Output: Table<Record>
Platforms: Linux, macOS
JC baseline: 1.26.0
Raw mode: yes
Streaming: no
```

A recognized SCOC parser must not be highlighted as an unknown executable command simply because it is not a program on `$PATH`.

## 21. Compatibility baseline and versioning

SCOC has its own semantic version, independent from its JC compatibility target and any individual JC parser version.

Example:

```text
SCOC library:      0.1.0
JC baseline:       1.26.0
JC df parser:      upstream parser-specific version
```

The initial pinned baseline is:

```toml
project = "jc"
version = "1.26.0"
commit = "73fa7d5572dd730076723bd6280786bb9101d32f"
```

Future SCOC releases may deliberately advance the JC baseline, but tests must never silently follow JC `master`.

## 22. Parser parity matrix

`compatibility/parser-matrix.toml` is the machine-readable source of truth for migration status.

Example shape:

```toml
[baseline]
jc_version = "1.26.0"
jc_commit = "73fa7d5572dd730076723bd6280786bb9101d32f"

[parsers.df]
status = "compatible"
platforms = ["linux", "macos"]
raw = true
streaming = false
jc_parser = "df"

[parsers.ping]
status = "compatible"
platforms = ["linux", "macos"]
raw = true
streaming = true
jc_standard = "ping"
jc_streaming = "ping-s"

[parsers.ipconfig]
status = "deferred-windows"
platforms = ["windows"]
```

Allowed states should be explicit rather than inferred from percentages:

- `unported`;
- `in-progress`;
- `compatible`;
- `compatible-with-documented-deviation`;
- `deferred-windows`;
- `deprecated`.

Documentation and release summaries should be generated from this matrix where practical.

## 23. Compatibility test strategy

Every parser must have both native Rust tests and differential compatibility tests.

### 23.1 Native fixture tests

Relevant upstream JC fixture inputs are vendored into the SCOC repository with provenance and license information. Expected JSON outputs are generated from the pinned baseline.

Conceptually:

```text
tests/fixtures/jc-1.26.0/linux/df/
├── ubuntu-basic.input
├── ubuntu-basic.expected.json
├── ubuntu-basic.raw.expected.json
└── ubuntu-basic.meta.toml
```

Ordinary `cargo test` runs only Rust/SCOC and local fixtures. It does not require Python, JC, or network access.

### 23.2 Differential harness

A separate compatibility harness feeds identical fixtures into pinned JC and SCOC, then structurally compares the resulting values.

```text
fixture
  ├──→ JC 1.26.0 → expected structured JSON
  └──→ SCOC      → actual serde_json::Value
                         ↓
                  structural comparison
```

The harness is a development/release gate, not a runtime dependency.

Parser-specific normalization/exclusion is permitted only for genuinely environment-dependent fields and must be explicitly documented in compatibility metadata.

### 23.3 Definition of compatible

A parser may be marked `compatible` only after the relevant tests cover, as applicable:

- normalized output;
- raw output;
- field names and types;
- empty input;
- malformed input behavior;
- platform-specific fixtures;
- parser options;
- streaming record boundaries;
- ordering;
- ignore-error behavior;
- final summary records;
- documented edge cases from upstream tests.

A parser that intentionally differs is marked `compatible-with-documented-deviation`, with the JC behavior, SCOC behavior, and reason recorded explicitly.

## 24. Licensing and provenance

The pinned upstream JC source is MIT licensed, copyright Kelly Brazil. Any copied or substantially derived source, fixtures, or documentation must retain the applicable MIT notice.

SCOC should carry the upstream notice in a dedicated third-party license file such as:

```text
THIRD_PARTY_LICENSES/JC-MIT.txt
```

Vendored fixtures should record original upstream paths and the pinned baseline commit in fixture metadata or adjacent provenance documentation.

The compatibility objective is behavioral parity; direct source translation is not required and should not be the default implementation technique.

## 25. Parser migration workflow

Every parser port follows the same sequence:

1. Read the pinned JC parser implementation.
2. Read all corresponding upstream tests.
3. Inventory relevant upstream fixtures.
4. Generate/freeze expected outputs from the pinned JC baseline.
5. Add failing native SCOC compatibility tests.
6. Implement the parser behavior in Rust.
7. Add edge cases not adequately represented upstream.
8. Run native Rust tests.
9. Run the differential harness.
10. Mark compatibility status only after both test layers pass.

This prevents an implementation agent from reading only a Python parser module and missing behavioral requirements encoded in years of tests and fixtures.

## 26. Migration phases

The full JC parser inventory is too large for one implementation plan. SCOC is one product but is delivered through multiple specs and plans.

### 26.1 Master architecture

This document defines contracts that later specs should not casually redefine:

- SCOC crate boundary;
- parser API;
- value representation;
- registry semantics;
- `from` resolution;
- namespaces;
- compatibility policy;
- streaming behavior;
- custom parser ownership;
- testing strategy.

### 26.2 First implementation spec: core + Spar bridge

The first implementation spec should prove the architecture with six representative parsers:

- `df` — normalized table parser;
- `ps` — process/table complexity;
- `ls` — filesystem-oriented variable output;
- `ping` — streaming parser;
- `fstab` — file-oriented parser;
- `env` — simple key/value parser.

The first milestone includes:

- standalone `scoc` Rust crate/repository;
- parser trait and registry;
- descriptors, options, and errors;
- `serde_json::Value` output;
- streaming interface;
- JC baseline metadata;
- parser parity matrix;
- native fixture harness;
- differential harness;
- Spar SCOC dependency;
- higher-level structured decoder registry;
- `from name(...)` grammar and namespaces;
- direct Value-to-Spar conversion;
- streaming bridge and cancellation integration;
- existing Sparsh structured rendering path;
- `spar-ls` decoder completion/hover/options/diagnostics for implemented decoders;
- registry slot for future custom parsers.

The first milestone does not need to implement the full `parser Identifier { ... }` language feature.

### 26.3 Parser foundation spec

A later spec ports foundational parsers that exercise reusable primitives, for example:

- `asciitable` / `asciitable-m`;
- `kv` / `kv-dup`;
- `ini` / `ini-dup`;
- `hosts`;
- `authorized-keys`;
- `crontab`;
- `passwd` / `group` / `shadow`;
- `os-release` / `lsb-release`.

Utilities are extracted based on demonstrated reuse.

### 26.4 High-value Linux coverage

A later spec expands daily-driver Linux command coverage, using the pinned inventory rather than an informal list. Likely categories include filesystem/storage, process/system, package managers, service tools, and common core utilities.

### 26.5 Networking and streaming closure

A later spec systematically closes networking and streaming parity, including standard/streaming parser pairs such as `ping`, `traceroute`, `git-log`, `iostat`, `mpstat`, `pidstat`, `rsync`, `top`, and syslog variants where they exist in the pinned inventory.

### 26.6 macOS closure

macOS is a first-class initial target and gets dedicated compatibility closure work, including native CI and macOS-specific command/output variants. A parser is not labeled macOS-compatible merely because it compiles on macOS.

### 26.7 Linux/macOS long tail

The remaining Linux/macOS inventory is ported in manageable waves until the parity matrix reaches the project's target coverage.

### 26.8 Windows later

Windows-specific parser parity is a separate later project. Windows parsers may remain represented in the inventory as `deferred-windows` until that work begins.

## 27. CI and verification

SCOC normal CI should include at least:

```text
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Compatibility CI additionally:

- installs the pinned JC baseline;
- runs the differential corpus;
- validates parser-matrix claims;
- runs relevant jobs on Linux and macOS.

Fixture-only tests may be platform-independent, but platform compatibility claims require evidence on the declared target platform where practical.

The project must not treat a successful compile as parser compatibility evidence.

## 28. Experimental and stable status

The Spar/Sparsh integration may become available experimentally before full parser parity is complete.

The core integration can leave experimental status when:

- the registry API is stable enough for consumers;
- decoder resolution is stable;
- streaming cancellation works;
- compatibility harnesses are reliable;
- Linux/macOS CI is healthy;
- decoder metadata is stable enough for `spar-ls`;
- representative batch and streaming parsers are proven.

Stable architecture does not mean complete JC parity. Parser coverage remains independently tracked by the parity matrix.

## 29. Non-goals

This master design explicitly does not make the following SCOC responsibilities:

- command execution;
- terminal table rendering;
- Reedline integration;
- Spar syntax implementation inside SCOC;
- a dependency from SCOC to Spar;
- JSON text serialization solely to communicate with Spar;
- Python or JC as a runtime dependency;
- a standalone SCOC CLI as a version-one requirement;
- native dynamic plugin loading;
- a SCOC-specific package manager;
- automatic execution of parsers from arbitrary plugin directories;
- automatic parser guessing based on the command name;
- Windows parity in the initial platform milestone;
- silent renaming of JC-compatible fields;
- claiming compatibility without fixture and differential evidence.

## 30. End-to-end examples

### 30.1 Batch parser

```spar
df | from df
```

Flow:

```text
df command
   ↓ stdout bytes
spar-process
   ↓
from df → resolves scoc::df
   ↓
scoc::parse("df", ...)
   ↓
serde_json::Value
   ↓ direct conversion
Spar Table<Record>
   ↓
Sparsh structured renderer
```

### 30.2 Structured processing

```spar
df
| from df
|> where(fn(row) => row.use_percent > 80)
|> select(["filesystem", "mounted_on", "use_percent"])
```

SCOC ends at structured conversion. `where` and `select` remain ordinary Spar structured operations.

### 30.3 Streaming parser

```spar
ping 1.1.1.1
| from ping
|> take(5)
```

Spar sees a live source, the `ping` descriptor advertises streaming, and `streaming` defaults to `auto`. Spar obtains a SCOC stream parser, feeds stdout chunks through `push`, converts emitted `serde_json::Value` items to Spar values, calls `finish` at EOF, and propagates cancellation after `take(5)` is satisfied.

### 30.4 Forced buffered behavior

```spar
ping 1.1.1.1 | from ping(streaming: false)
```

Spar buffers the command output and then calls batch `parse()`.

### 30.5 Existing native codec

```spar
cat users.csv | from csv
```

This resolves to `codec::csv`, preserving existing Spar behavior.

### 30.6 Explicit JC-compatible format parser

```spar
cat users.csv | from scoc::csv
```

This explicitly uses SCOC's JC-compatible CSV parser once that parser is implemented.

### 30.7 Custom parser naming

```spar
parser CompanyStatus {
    description: "Parse the company status command";
    name: "company-status";
    output: table;

    options {
        verbose: bool = false;
    }

    function parse(input, options) {
        // returns structured Spar data
    }
}
```

Usage:

```spar
company status | from company-status
company status | from custom::company-status
```

The declaration symbol follows normal Spar naming rules; the string field is only the external decoder registration name.

## 31. Final ownership model

```text
spar-process
    owns command execution, byte streams, cancellation
          │
          ▼
spar
    owns `from` grammar and decoder resolution
          │
          ├──────────────┐
          ▼              ▼
     native codecs      SCOC
                         │
                         ▼
                  serde_json::Value
          │              │
          └──────┬───────┘
                 ▼
          Spar structured values
                 │
                 ▼
              |> stages
                 │
                 ▼
              Sparsh
              renders

spar-ls
    understands decoder metadata for completion,
    hover, diagnostics, and semantic tokens.
```

The design principle is:

> **SCOC parses. Spar integrates. `spar-process` streams. Sparsh renders. `spar-ls` understands. JC provides the compatibility oracle.**

## 32. Design review gate

This document completes the written-design stage only. It does not authorize implementation.

After the user reviews and approves this written spec, the next process step is to create a separate detailed implementation plan for the first SCOC milestone: core engine, Spar bridge, compatibility harness, and the six representative parsers. Implementation begins only after that plan is reviewed and the execution method is explicitly selected.
