# Command Runner V2 — Verification

Verified 2026-08-30 on `feat/command-runner`, after all V2 implementation
commits (`72e22d3`..`690a2fb`, plus the formatting/typo cleanup and
documentation commits on top). Implementation followed
`docs/superpowers/plans/2026-08-30-command-runner-v2.md` Tasks 1-9; this
records Task 10 (documentation) and Task 11 (verification).

## Static checks

```bash
cargo fmt --check
```
Exit 0, clean.

```bash
cargo check
```
Clean build, no warnings.

```bash
cargo clippy --all-targets --all-features -- -D warnings
```
30 pre-existing findings, all in files V2 never touches
(`evaluator.rs`, `loader.rs`, `parser.rs`, `resolver.rs`, `typechecker.rs`,
`lexer.rs`, `tests/parser_tests.rs`) — confirmed identical on a fresh clone
of `dev` before any command-runner work. Zero new findings from V1 or V2
runner/task-lowering/CLI code.

## Test suite

```bash
cargo test
```

| Suite | Result |
| --- | --- |
| `unittests src/lib.rs` | 647 passed, 0 failed |
| `unittests src/main.rs` | 19 passed, 0 failed |
| `tests/cli_golden.rs` | 1 passed, 0 failed |
| `tests/compiler_facade.rs` | 2 passed, 0 failed |
| `tests/conformance_corpus.rs` | 1 passed, 0 failed |
| `tests/task_cli.rs` | 19 passed, 0 failed |
| `tests/task_lowering.rs` | 12 passed, 0 failed |
| doctests | 0 |

**701 tests total, 0 failed, 0 ignored.** (V1 baseline before any
command-runner work: 599. After V1: 664. After V2: 701 — net +37 for V2's
attributes, variadic/default parameters, shebang scripts, dotenv, shell
override, discovery, and grouped/`--choose`/`show`/`dump` CLI surface.)

## Manual cross-feature smoke test

Run against a disposable `/tmp` fixture (not checked in), one file
combining every V2 feature: `@LoadEnv` + a `.env` providing `GREETING`,
a default task, a `private`+`group`+`confirm` task, a task with a default
and a variadic parameter plus a `shell:` override, and a shebang script
task.

- `spar tasks` from a directory two levels below the fixture found
  `SparMake.spar` by walking up parents, listed tasks grouped
  (`Ungrouped` first, then `release`), private task hidden.
- `spar tasks --all` showed the private task alongside the public ones.
- `spar run script` executed the shebang task as one script; its
  interpolated `$GREETING` resolved to the `.env` value even though the
  task declared no `env: {}` of its own — confirms dotenv values reach
  every task by default, task `env:` still wins where declared.
- `spar run deploy production --force blue` rendered
  `deploy production --force blue` (default `environment` overridden,
  `*extra` variadic captured both trailing args, space-joined) via the
  `shell: ["sh", "-c"]` override.
- `spar show deploy production --force blue` printed the same resolved
  command with no process spawned.
- `spar dump` produced valid JSON; the `Build` task's `environment` object
  included the dotenv-sourced `GREETING`, confirming lowering order.
- `spar run --choose` (stdin: `2`) listed all three public tasks numbered
  and grouped, selected `script` by number, and ran it.
- `spar run release` (stdin: `n`) printed `Really release? [y/N] `, then
  `error: task Release aborted` and exited non-zero — no command ran.

## Review

Self-reviewed (master, in-context — no reviewer agent spawned; per
`agent-swarm-dev`'s risk scoring this V2 diff scores in the "one reviewer"
band: a public CLI/interface change plus a large semantic diff, offset by
strong existing test coverage) against the pre-V2 commit `2c0a09e`, focused
on the higher-risk surfaces: process spawning and shebang/script handling
in `executor.rs`/`shell.rs`, argument binding in `graph.rs`, and the
hand-rolled parser in `dotenv.rs`. One real finding, fixed: `tasks.md`
understated `confirm` semantics — every `confirm`-marked task in a plan is
prompted upfront, in dependency order, *before any command runs*; declining
any one aborts the whole plan, not just that task and its dependents. No
other correctness issues found. Everything else already had direct test
coverage (defaults/variadic binding, OS mismatch on both a requested task
and a dependency, quiet-vs-suppressed-output, dry-run skipping both
confirmation and process spawn, custom shell argument order, Windows
shebang token parsing).

## Scope delivered vs. `docs/superpowers/specs/2026-08-30-command-runner-v2-design.md`

All user-selected V2 buckets are implemented: recipe attributes
(`private`/`group`/`confirm`/`os`), default and variadic parameters,
shebang/script recipes, `.env` loading via `@LoadEnv`, per-task shell
override, richer grouped `spar tasks`, built-in `--choose`, `spar show`,
`spar dump`, and `SparMake.spar` discovery. Provenance updated in
`just-extraction-map.md` (new "V2 additions" section, V1 rows left intact)
and `UPSTREAM-JUST.md`. Deliberately not implemented, per the spec: Just
aliases, an export-all-vars setting, and an external (`fzf`-style) chooser
dependency.
