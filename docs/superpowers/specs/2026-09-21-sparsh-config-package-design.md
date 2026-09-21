# Sparsh config directory as a Spar package

Date: 2026-09-21
Status: approved design, not implemented

## Goal

Make `~/.sparsh` a full Spar package (manifest, lockfile, `src/` modules) so users can add
dependencies from GitHub or local paths and import them in their shell config, at the live
prompt, and in scripts. A user could publish a package called `my-tools`, add it as a
dependency, and write `import pkg { dismantler } from "my-tools";` anywhere in the shell.

Spar already has the package system: `spar.package.spar` manifests (`kind = "config"` is
supported), `spar.package.lock.spar`, `github:` and `path:` dependency sources, a global
store, and `import pkg`. This work wires sparsh to it. It does not build a new package
manager.

There are no existing users besides the author, so the layout is standardized and migrated
outright; no flat-layout compatibility path is kept.

## Layout

```
~/.sparsh/
  spar.package.spar        kind = "config", name = "sparsh-config", entry = "src/config.spar"
  spar.package.lock.spar   generated, starts empty
  src/
    config.spar            (was sparsh.spar) the auto-loaded entry point
    functions.spar
    sparsh-types.spar
    ...any other user modules
```

The config path constant changes from `~/.sparsh/sparsh.spar` to `~/.sparsh/src/config.spar`.
Today this lives in `sparsh-core/src/config.rs` (`config_path`) and is the only place that
knows the location. The bundled template in `sparsh-core/config-package/` is updated to the
same layout and is what fresh installs are seeded from.

## Startup and migration

At startup sparsh checks for `~/.sparsh/spar.package.spar`.

- **Present:** load the manifest, resolve `entry`, and use the lockfile for `import pkg`.
- **Absent, directory has a legacy flat layout (`sparsh.spar` exists):** migrate.
  1. Copy the whole directory to `~/.sparsh.bak/`. If that path already exists, do not
     migrate; print `~/.sparsh.bak exists; move it and restart to migrate` and start from
     the flat layout.
  2. Move `sparsh.spar`, `functions.spar`, `sparsh-types.spar`, and any other `*.spar` files
     into `~/.sparsh/src/`, renaming `sparsh.spar` to `config.spar`. Relative imports keep
     working because the files move together.
  3. Write `spar.package.spar` and an empty `spar.package.lock.spar`.
- **Absent, no directory or no config (fresh install):** seed the bundled template. No backup
  is made because there is nothing to lose.
- Migration never overwrites an existing file. Migration is idempotent: once the manifest
  exists, startup never migrates again.
- If any step fails, restore from `~/.sparsh.bak`, start a minimal session, and report the
  failing step. A half-migrated directory is never left in place.
- Startup never uses the network. It reads only the manifest, lockfile, locked `path:`
  dependencies, and the global store.

## Imports

The interactive session already supports `import pkg` and exposes
`with_bundled_package_root`. The session is given `~/.sparsh` as its root package, so
`import pkg { x } from "alias"` resolves through the manifest's `Dependencies` and the lockfile.
This behaves identically in `config.spar`, at the live prompt, and in `.spar` scripts run by
the shell. Imported names persist in the session like any other binding. `std/*` imports are
unchanged.

Errors:
- Alias is not a dependency: `"my-tools" is not a dependency of ~/.sparsh; run: pkg add my-tools github:owner/repo@1.0`.
- Locked package is missing from the store: the shell starts anyway; that import fails with
  `run: pkg install`; other imports are unaffected.

## The `pkg` builtin

A sparsh builtin that runs the `spar::package::commands` functions (the same ones the `spar`
CLI calls) against `~/.sparsh`, from any working directory:

- `pkg add <alias> <request>`
- `pkg remove <alias>`
- `pkg install [--offline]`
- `pkg update [alias]`
- `pkg tree`

Requests use the existing formats: `github:owner/repo@1.4.0`, `github:owner/repo#branch`,
`path:../local-dir`. After a successful `add`, `remove`, `install`, or `update`, the running
session reloads its package graph so new imports work without a restart. Failures leave the
session graph unchanged; spar's `add` is already atomic (nothing written on failure).

## Error handling

A bad config package never bricks the shell. Every failure ends with a working prompt and one
clear message.

| Failure | Behavior |
|---|---|
| Backup path already exists | Skip migration, load flat layout if present, print the notice above. |
| Copy or move step fails | Restore from `~/.sparsh.bak`, minimal session, report failing step. |
| Manifest malformed, or a field is not a plain literal string | Minimal session, compiler-style error with file and line. |
| Lockfile malformed or out of sync with manifest | Same, plus `run: pkg install`. |
| Locked package missing from store | Shell starts; that import fails with `run: pkg install`. |
| `import pkg` names a non-dependency | Error naming the alias with the `pkg add` hint. |
| `pkg add` fails partway | Nothing written; session graph unchanged. |
| Config fails to compile or run | Unchanged from today: show errors, fall back to defaults. |

## Testing

All tests use temporary `HOME` directories. Nothing touches the real `~/.sparsh`. GitHub
sources are never hit from tests; dependency fixtures use `path:` packages.

1. **Migration:** flat layout becomes package layout; backup is byte-identical to the original;
   a second run does nothing; injected failures (unwritable directory, pre-existing backup)
   leave the original intact; fresh install seeds the template with no backup.
2. **Loading:** config resolves from `src/config.spar`; sibling imports work after the move;
   malformed manifest or lock yields the minimal session and a message.
3. **Imports:** a tiny local `path:` package exporting `dismantler` imports in `config.spar`, at
   the prompt, and in a script; unknown alias shows the hint; missing store entry shows the
   `pkg install` hint.
4. **`pkg` builtin:** `add`, `remove`, `install --offline`, `update`, `tree` behave as the spar
   CLI does, run against `~/.sparsh` regardless of cwd, and reload the session graph.
5. **Regression:** the existing sparsh suite stays green, including prompt-config tests that
   currently hard-code `.sparsh/sparsh.spar`.

## Out of scope

A package registry, an auto-imported prelude, install-time code execution (the package docs
already promise none), and GitHub network tests.
