# Just Runner Integration: Milestones 1–3

## Goal

Run a Spar task through copied Just runtime code. Do not rebuild Just. Scope ends after milestones 1–3.

## Architecture

Copy the Just library source into `crates/command-runner` inside the Spar repository. Preserve its source layout and CC0 license. Keep the copied Just frontend private and unused; do not expose Just syntax through Spar.

The Spar crate uses the copied crate by a path dependency. A small public bridge in the copied crate accepts cooked task data: task name, final command lines, and the minimum execution options. The bridge constructs Just runtime structures inside their crate, where private fields remain accessible, and invokes the existing Just recipe/dependency/process path.

Data flow:

```text
Spar source -> Spar lexer/parser -> Spar task AST -> Spar value interpolation
            -> cooked bridge input -> copied Just runtime -> shell/process
```

## Milestones

1. The copied command-runner crate and Spar compile together. Changes to copied source stay minimal: crate metadata, visibility, embedded-file references, and bridge module.
2. An integration test constructs `echo hello` as cooked task data and executes it through the copied Just runtime.
3. Spar parses `task [Hello] { run { echo hello; }; }` and the same bridge executes it. This milestone may add only the smallest CLI entry needed by the end-to-end test; the full Clap redesign belongs to milestone 6.

## Spar Syntax in Scope

Only a named task with a `run` block is required. Dependencies, parameters, task environment, cwd, shell settings, dotenv, and LSP work remain later milestones. Command text is represented distinctly from normal Spar expressions, but `${...}` uses Spar expression parsing/evaluation before bridge input is built.

## Errors

Spar lex/parse/evaluation errors use Spar diagnostics. Runtime launch and exit failures come from copied Just behavior and are converted at the bridge boundary into a stable public error type without losing the message or exit status.

## Tests and Proof

- Baseline Spar tests remain green.
- Copied Just crate passes `cargo check` and relevant retained tests.
- Bridge test proves output and success for `echo hello`.
- Lexer/parser tests prove the task syntax and reject malformed blocks.
- End-to-end test proves parsed Spar source reaches the copied runner.
- Run formatting, checking, Clippy, and tests appropriate to the changed crates.

## Non-Goals

Milestones 4–8, Just public syntax, a second evaluator, runtime redesign, broad pruning of copied Just files, and changes to `~/Projects/just`.
