# Spar packages

A package is reusable Spar code, versioned and shared via GitHub or a local
path. This is the reference; see the README's [Packages](../README.md#packages)
section for a quick tour.

## Manifest

`spar.package.spar`, written in Spar itself, holds exactly three sections —
`[Package]` (required), `[Dependencies]` (optional), `[Overrides]` (optional)
— and every field in them must be a literal string: no `${...}`
interpolation, no function calls, no references to other values. Parsing a
manifest never runs the resolver, type checker, or evaluator.

```spar
[Package] -> SparPackage {
    name: "my-app";
    version: "1.0.0";
    kind: "application";
    entry: "src/main.spar";
};

[Dependencies] {
    http: str = "github:owner/spar-http@1.4.0";
};

[Overrides] {
    http: str = "path:../spar-http";
};
```

`kind` is one of `application`, `library`, or `config`, and picks the
conventional `entry` when one isn't given explicitly: `src/main.spar`,
`src/lib.spar`, `src/config.spar` respectively. An explicit `entry` is always
authoritative.

The reserved filename implicitly preloads `SparPackage`; no import or copied
schema is needed. `spar check` validates the shape, and `spar-ls` exposes its
fields and allowed `kind` values for completion and hover. The package tools
emit this typed form. Older unbound `[Package] { ... }` sections are still
accepted when reading a manifest.

`[Overrides]` swaps a dependency's resolution for local development without
touching the declared `[Dependencies]` entry — every alias in `[Overrides]`
must already exist in `[Dependencies]`.

## Dependency requests

- `github:owner/repo@1.4.0` — an exact version. A bare version always means
  exactly that version, never "compatible with"; write `github:owner/repo@^1.4`
  for a SemVer range.
- `github:owner/repo#branch-or-commit` — a branch name or exact commit.
  Resolving either one still locks to one concrete commit.
- `path:../local-dir` — a local directory containing its own
  `spar.package.spar`.

## Imports

No new import syntax. A filesystem-like string (starts with `.`/`/`, or ends
in `.spar`) resolves exactly as it always has. A bare word resolves through
the current project's `[Dependencies]`/lockfile instead:

```spar
import "http" as http;
import { get, post } from "http";
```

## Lockfile and global store

`spar add`/`spar install`/`spar update` write `spar.package.lock.spar` — the
exact resolved dependency graph as typed Spar source, sorted deterministically
so diffs stay readable. Its root is `[Lock] -> SparPackageLock`; built-in
`SparLockedPackage` and `SparLockedDependency` shapes validate package
identities and graph edges. Commit it to version control; you shouldn't need
to hand-edit this generated file.

Immutable resolved package contents live once per machine, deduplicated by exact
revision, under `$XDG_DATA_HOME/spar/store` (`~/.local/share/spar/store` by
default) — never inside a project directory. Disposable fetch metadata lives
under `$XDG_CACHE_HOME/spar/`.

Local `path:` dependencies are live development checkouts and are not copied
into either the application or the immutable store. Keep them as separate
directories/repositories (a sibling such as `path:../spar-http` is typical).

**Ordinary execution never touches the network.** `spar check`, `spar emit`,
`spar exec`, task runs, and ordinary imports only ever read
`spar.package.lock.spar`, locked live paths, and the global store. Network
access is confined to the explicit commands below.

## Commands

| Command | Network | What it does |
|---|---|---|
| `spar init [name] [--app\|--lib\|--config]` | no | Creates `spar.package.spar` and a stub entry file. Refuses to overwrite an existing manifest. |
| `spar add <alias> <request>` | yes | Adds/updates one dependency, resolves the whole graph, materializes it into the store, writes the manifest and lockfile together. Nothing is written if resolution fails partway. |
| `spar remove <alias>` | yes* | Drops a dependency (and any override naming it) and re-resolves — anything no longer reachable just doesn't appear in the new lock. The store itself is never touched; another project may still use the same snapshot. |
| `spar install` | maybe | Materializes every already-locked package at its exact recorded revision — never re-resolving a version requirement or branch, so a moved upstream tag can't silently change what installs. Only fetches for snapshots the store doesn't already have. |
| `spar install --offline` | no | Same, but fails clearly if anything's still missing instead of touching the network. |
| `spar update [alias]` | yes | Re-resolves against the manifest's *current* requests — the explicit, opposite operation to `install`. This is the one command that intentionally moves a lock forward. |
| `spar tree` | no | Prints the resolved dependency tree from `spar.package.lock.spar`. |

\* `remove` only needs the network if the remaining dependency graph still
has something to (re-)resolve.

## Security

Phase 0 packages are Spar source and manifest metadata only. Installing a
package never executes any of its code — there are no install lifecycle
scripts (`postinstall` and friends don't exist here), and no native
`.so`/`.dll` package extensions.

## Deliberately out of scope for this phase

- A first-party standard library package (the package manager and host
  registry make one possible; building it is a separate project).
- Package sources other than GitHub and local paths (the `PackageSource`
  abstraction has room for a future `git:`/`gitlab:`/registry provider).
- Partial re-resolution (`spar update <alias>` re-resolves the whole graph
  today, not just that one dependency's subtree).
