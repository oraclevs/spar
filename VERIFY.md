# Task 15 Verification

Run from the ecosystem root after applying the ZIP.

```zsh
cd ~/Projects/Rust/occ_lang/spar-process

cargo fmt
cargo fmt --check
cargo check

cargo test --test streaming_output
cargo test

cargo clippy --all-targets --all-features -- -D warnings
```

## Expected focused behaviors

`cargo test --test streaming_output` must pass tests covering:

1. stdout arrives before a still-running process exits;
2. stdout and stderr are separate by default;
3. pipeline stderr does not contaminate stdout;
4. process-group cancellation terminates a streaming pipeline;
5. zero chunk/channel sizes fail with `InvalidInput` before spawn;
6. configured chunk size bounds delivered chunks without altering bytes;
7. explicit stdout redirection produces file output rather than streamed output;
8. explicit `2>&1` semantics merge stderr into the stdout stream;
9. dropping a live `ProcessStream` kills and reaps it;
10. streaming keeps per-process pipeline statuses.

## Optional downstream compatibility checks

After `spar-process` is green, these checks are useful because `spar` and
`sparsh` depend on it, but failures in still-unverified Plan 2 work should be
reported separately from Task 15:

```zsh
cd ~/Projects/Rust/occ_lang/spar
cargo check

cd ../sparsh
cargo check
```

Task 15 is complete only after the focused test, the full `spar-process` test
suite, and Clippy all pass locally.
