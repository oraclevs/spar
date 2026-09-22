# Task 15 — Streaming Process Output API

Global roadmap task: **15**
Plan: **Plan 3 — Process Bridge + Sparsh**

## Scope

This patch changes only `spar-process` and keeps the approved repository boundary:

- `spar-process` owns OS process spawning, byte streaming, process groups,
  cancellation, and process statuses.
- It does **not** depend on Spar runtime `Value`, `Table`, `Stream`, parsers, or
  Sparsh.
- It does not implement Task 16 byte/value conversion or Task 17 mixed-pipeline
  planning.

## Added

- `spar-process/src/stream.rs`
  - `StreamingOptions`
  - bounded `sync_channel` transport
  - `ProcessOutputChunk::{Stdout, Stderr}` tagging
  - `ProcessStream`
  - `stream_command`
  - `stream_pipeline`
  - graceful process-group cancellation
  - force cleanup on dropped live streams
  - per-process and aggregate pipeline status preservation
  - `collect()` compatibility path for callers that need materialized bytes
- `spar-process/tests/streaming_output.rs`
  - output is observable before process exit
  - stdout/stderr remain separate by default
  - intermediate pipeline stderr remains tagged separately
  - explicit redirection still wins
  - explicit stderr-to-stdout merge is preserved
  - chunk size bounds delivered chunks
  - zero-sized streaming buffers are rejected before spawn
  - cancellation targets the whole process group
  - dropping a live stream kills and reaps the child
  - per-process pipeline statuses survive streaming

## Modified

- `spar-process/src/exec.rs`
  - existing internal spawn/redirection/environment helpers are exposed as
    `pub(crate)` so the streaming executor reuses the same Unix semantics
    instead of duplicating them.
- `spar-process/src/lib.rs`
  - exports the new streaming module.
- `spar-process/README.md`
  - documents blocking + streaming ownership and architecture boundary.

## Architecture notes

- One bounded queue carries both stdout and stderr chunks, tagged by source.
  This keeps the streams logically separate while ensuring a consumer can drain
  both without creating a dual-channel deadlock.
- Reader threads block on a bounded `sync_channel`, which applies backpressure
  rather than accumulating unbounded process output.
- No hidden `/bin/sh -c` is introduced. Tests invoke `sh` explicitly where a
  test script is useful; production execution still calls the requested program
  directly.
- Process groups use the existing `spar-process::job` infrastructure.

## Files intentionally unchanged

- `spar`
- `spar-command`
- `sparsh`
- `spar-ls`

## Verification status

Rust execution is **unverified in the assistant environment** because no
`cargo`, `rustc`, or `rustfmt` executable is available there. The user must run
`VERIFY.md` locally before Task 15 is marked complete.
