# Upstream Just Provenance

## Donor record

| Field | Value |
| --- | --- |
| Project | Just |
| Upstream URL | `https://github.com/casey/just.git` |
| Audited commit | `b20386abdbae867a49cdff6c3c0f2b547faa9b23` |
| License | `CC0-1.0` (`LICENSE`) |
| Extraction date | `2026-08-29` |
| Local donor | `/home/occ/Projects/just` (read-only) |

## Test baseline recorded for the audited donor

- Unit tests: **575 passed, 0 failed**.
- Integration tests: **1,836 passed, 18 ignored, 0 failed**.

These counts are the audit baseline specified for commit
`b20386abdbae867a49cdff6c3c0f2b547faa9b23`; they are not an assertion about a
future Spar implementation.

## Concepts adapted into Spar

Spar may adapt the behavioral ideas of: constructing a shell command,
platform-specific shell argument handling, inheriting standard input/output/
error, reporting nonzero child status, applying environment values to the
child, setting a working directory, and keeping dry-run from spawning a
child. The relevant source units and deliberate exclusions are recorded in
[just-extraction-map.md](just-extraction-map.md).

No Just source is copied or linked. Spar reimplements the approved V1 behavior
against its own parser-independent runner IR. Just's parser/compiler, recipe
AST, evaluator, module/import system, settings, formatter, CLI, caches,
dotenv handling, aliases, Make-like target behavior, and completion machinery
are outside the extraction boundary.
