# Sparsh Config Directory as a Spar Package Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `~/.sparsh` becomes a Spar package (manifest, lockfile, `src/`), so users can add GitHub or local dependencies with a `pkg` builtin and `import pkg { x } from "alias"` in their config, at the live prompt, and in scripts.

**Architecture:** A new `config_home` module in `sparsh-core` owns the layout, one-time migration (backup, move, manifest, lock, rollback) and fresh-install seeding. `ShellSession::reload_config` runs it, builds a `spar::Engine` with a `ModuleLocator` from `~/.sparsh/spar.package.lock.spar`, and evaluates `~/.sparsh/src/config.spar`. A `pkg` builtin wraps `spar::package::commands` against `~/.sparsh` and requests a config reload. Two small hooks are added to `spar` (`Engine::with_locator`, `Engine::with_package_command`).

**Tech Stack:** Rust; crates `spar` (path dep) and `sparsh-core`; existing `spar::package` API.

**Spec:** `spar/docs/superpowers/specs/2026-09-21-sparsh-config-package-design.md`

## Global Constraints

- Layout (exact): `~/.sparsh/spar.package.spar` (`kind = "config"`, name `sparsh-config`, entry `src/config.spar`), `~/.sparsh/spar.package.lock.spar`, `~/.sparsh/src/config.spar`, other modules alongside in `src/`.
- Backup directory (exact): `~/.sparsh.bak`. Migration refuses to run if it already exists and prints exactly: `~/.sparsh.bak exists; move it and restart to migrate`.
- Migration never overwrites an existing file, is idempotent (never runs once the manifest exists), and on any failure restores the original tree (the failed tree is renamed aside to `~/.sparsh.failed-migration`, never deleted).
- Startup and imports never use the network: only the manifest, lockfile, locked `path:` deps and the global store are read.
- A bad config package never bricks the shell: every failure ends with a working prompt and one clear message.
- `pkg` subcommands (exact): `pkg add <alias> <request>`, `pkg remove <alias>`, `pkg install [--offline]`, `pkg update [alias]`, `pkg tree`. Requests: `github:owner/repo@1.4.0`, `github:owner/repo#branch`, `path:../dir`.
- Tests use temporary `HOME` directories only; never touch the real `~/.sparsh`; never hit GitHub (use `path:` fixtures).
- No new external crates.
- Never add attribution lines or co-author trailers to commits. The repos have unrelated uncommitted changes: always `git add` explicit paths and `git commit -- <paths>`, never `git add -A` / `git commit -a`.
- Repos: `spar/` (Task 1) and `sparsh/` (Tasks 2-5) are separate git repos. Run cargo from inside the crate directory (`sparsh/` is a workspace: `cargo test -p sparsh-core`).

---

### Task 1: Spar hooks — `Engine::with_locator` and package command name in hints

**Files:**
- Modify: `spar/src/compiler.rs` (`CompileOptions` ~line 94 + `Default` ~117 + loader construction ~213)
- Modify: `spar/src/loader.rs` (`ImportLoader` struct + builder near `with_locator` line 59; hints at ~132, ~149, ~1160)
- Modify: `spar/src/engine.rs` (two builder methods after `with_bundled_package_root`)
- Test: `spar/tests/package_imports.rs`

**Interfaces:**
- Produces: `spar::Engine::with_locator(self, locator: spar::package::ModuleLocator) -> Self`; `spar::Engine::with_package_command(self, name: impl Into<String>) -> Self`; `CompileOptions.package_command: String` (default `"spar"`); `ImportLoader::with_package_command(self, name: String) -> Self`.

- [ ] **Step 1: Write failing tests**

Append to `spar/tests/package_imports.rs`:

```rust
#[test]
fn engine_with_locator_resolves_package_imports() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());
    let engine = spar::Engine::default()
        .with_base_dir(temp.path())
        .with_locator(locator);
    let compilation = engine.emit_source("import pkg { get } from \"http\";\nvar x: str = get(\"a\");\n");
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
}

#[test]
fn unknown_package_alias_hint_names_the_configured_command() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());
    let engine = spar::Engine::default()
        .with_base_dir(temp.path())
        .with_locator(locator)
        .with_package_command("pkg");
    let compilation = engine.emit_source("import pkg { x } from \"nope\";\n");
    let text: String = compilation
        .errors
        .iter()
        .map(|error| format!("{error:?}"))
        .collect();
    assert!(text.contains("pkg add nope"), "{text}");
    assert!(!text.contains("spar install"), "{text}");
}

#[test]
fn default_package_command_is_spar() {
    let temp = tempfile::tempdir().unwrap();
    let (_lockfile, _store, locator) = http_dependency_fixture(temp.path());
    let engine = spar::Engine::default()
        .with_base_dir(temp.path())
        .with_locator(locator);
    let compilation = engine.emit_source("import pkg { x } from \"nope\";\n");
    let text: String = compilation
        .errors
        .iter()
        .map(|error| format!("{error:?}"))
        .collect();
    assert!(text.contains("spar add nope"), "{text}");
}
```
Check that `Engine::emit_source` returns `Compilation` with `.errors` (it does, `engine.rs:103`). If a `SparError`'s `Debug` output does not include the hint text, use its `hint` field instead: `SparError::ResolveError { hint, .. }` — match on it and assert on `hint.as_deref()`.

Run: `cd spar && cargo test --test package_imports engine_with_locator unknown_package_alias default_package_command 2>&1 | tail -15` → Expected: compile error (`with_locator` not found).

- [ ] **Step 2: Implement**

`compiler.rs`: add to `CompileOptions`:
```rust
    /// The command name package-import hints tell users to run (`spar` for
    /// the CLI; embedders such as Sparsh set their own, e.g. `pkg`).
    pub package_command: String,
```
and `package_command: "spar".to_string(),` in `Default`. Where the loader is built (~line 213) chain `.with_package_command(self.options.package_command.clone())`.

`loader.rs`: add field `package_command: String` to `ImportLoader` (initialized to `"spar".to_string()` in `ImportLoader::new`), plus:
```rust
    pub fn with_package_command(mut self, name: String) -> Self {
        self.package_command = name;
        self
    }
```
Replace the three hints:
- ~line 132 (no locator configured): `format!("run `{cmd} install` in a project with spar.package.spar, then try again")`
- ~line 149 (alias/module not found): 
```rust
                    hint: Some(format!(
                        "run `{cmd} add {alias} <source>` to add this dependency, or `{cmd} install` if the lockfile changed",
                        cmd = self.package_command,
                        alias = decl.path.split('/').next().unwrap_or(&decl.path),
                    )),
```
- ~line 1160 (package file missing): `format!("check the package alias/module path and run `{} install` if dependencies changed", self.package_command)` (this is inside a `&mut self`/`&self` method of the loader; if it is a free function, pass `&self.package_command` through).

`engine.rs` (after `with_bundled_package_root`):
```rust
    /// Routes explicit `import pkg` requests through `locator`'s lockfile and
    /// store, exactly as the `spar` CLI does for a project directory.
    pub fn with_locator(mut self, locator: crate::package::ModuleLocator) -> Self {
        self.options.locator = Some(locator);
        self
    }

    /// Sets the command name used in package-import hints (default `spar`).
    pub fn with_package_command(mut self, name: impl Into<String>) -> Self {
        self.options.package_command = name.into();
        self
    }
```
Fix any `CompileOptions { .. }` struct literals without `..Default::default()` the compiler reports.

- [ ] **Step 3: Run tests and commit**

Run: `cd spar && cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy --all-targets -- -D warnings 2>&1 | tail -3`
Expected: PASS. (Hint text elsewhere in existing tests that asserted `spar install` still passes because the default is `spar`; update an assertion only if a test pinned the old exact hint text.)

```bash
cd spar && git add src/compiler.rs src/loader.rs src/engine.rs tests/package_imports.rs
git commit -m "feat: Engine::with_locator and configurable package command in import hints" -- src/compiler.rs src/loader.rs src/engine.rs tests/package_imports.rs
```

---

### Task 2: `config_home` module — layout, migration, seeding

**Files:**
- Create: `sparsh/crates/sparsh-core/src/config_home.rs`
- Create: `sparsh/crates/sparsh-core/templates/config.spar`
- Modify: `sparsh/crates/sparsh-core/src/lib.rs` (add `mod config_home;`)
- Test: inside `config_home.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (all `pub(crate)` in `crate::config_home`):
  - `fn root_for_home(home: &Path) -> PathBuf` → `home/.sparsh`
  - `fn backup_for_home(home: &Path) -> PathBuf` → `home/.sparsh.bak`
  - `fn entry_for_home(home: &Path) -> PathBuf` → `home/.sparsh/src/config.spar` if it exists, else `home/.sparsh/sparsh.spar` if that exists (legacy kept), else `home/.sparsh/src/config.spar`
  - `fn store_for(environment: &EnvironmentService) -> spar::package::PackageStore` (XDG_DATA_HOME / XDG_CACHE_HOME from the shell environment, falling back to `$HOME/.local/share`, `$HOME/.cache`)
  - `fn locator_for(root: &Path, store: &PackageStore) -> Result<Option<spar::package::ModuleLocator>, String>`
  - `enum Prepared { AlreadyPackage, Seeded, Migrated, LegacyKept { notice: String } }`
  - `struct PrepareError { pub step: &'static str, pub message: String }` implementing `Display` as `cannot migrate ~/.sparsh ({step}): {message}` and `std::error::Error`
  - `fn prepare(home: &Path, store: &PackageStore) -> Result<Prepared, PrepareError>`
  - `const BACKUP_EXISTS_NOTICE: &str = "~/.sparsh.bak exists; move it and restart to migrate";`

- [ ] **Step 1: Write the template**

`sparsh/crates/sparsh-core/templates/config.spar`:
```spar
// Sparsh configuration. This file is the entry point of the ~/.sparsh Spar package.
// Add dependencies with `pkg add <alias> <source>` and import them:
//   import pkg { helper } from "my-tools";
// Declare `struct Config: SparshConfig { ... };` to configure aliases, prompt, history and keybindings.
```

- [ ] **Step 2: Write failing tests**

Create `config_home.rs` with only the test module first (types referenced below do not exist yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_store(dir: &Path) -> PackageStore {
        PackageStore::new(spar::package::StorePaths::new(
            dir.join("store-data"),
            dir.join("store-cache"),
        ))
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn flat_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".sparsh");
        write(&root.join("sparsh.spar"), "var a: int = 1;\n");
        write(&root.join("functions.spar"), "function f() -> int { return 1; };\n");
        write(&root.join("sparsh-types.spar"), "// types\n");
        write(&root.join("lib/util.spar"), "var u: int = 2;\n");
        write(&root.join("notes.txt"), "keep me\n");
        write(&root.join(".hidden"), "dot\n");
        home
    }

    #[test]
    fn migrates_flat_layout_into_a_package() {
        let home = flat_home();
        let store = test_store(home.path());
        let outcome = prepare(home.path(), &store).unwrap();
        assert_eq!(outcome, Prepared::Migrated);

        let root = home.path().join(".sparsh");
        assert_eq!(fs::read_to_string(root.join("src/config.spar")).unwrap(), "var a: int = 1;\n");
        assert!(root.join("src/functions.spar").is_file());
        assert!(root.join("src/sparsh-types.spar").is_file());
        assert!(root.join("src/lib/util.spar").is_file(), "directories holding .spar files move too");
        assert!(root.join("notes.txt").is_file(), "non-spar files stay");
        assert!(root.join(".hidden").is_file(), "dotfiles stay");
        assert!(!root.join("sparsh.spar").exists());

        let manifest_path = root.join("spar.package.spar");
        let manifest = spar::package::PackageManifest::parse(
            &fs::read_to_string(&manifest_path).unwrap(),
            &manifest_path,
        )
        .unwrap();
        assert_eq!(manifest.name, "sparsh-config");
        assert_eq!(manifest.kind, spar::package::PackageKind::Config);
        assert!(root.join("spar.package.lock.spar").is_file());
    }

    #[test]
    fn backup_is_a_byte_identical_copy_of_the_original() {
        let home = flat_home();
        prepare(home.path(), &test_store(home.path())).unwrap();
        let backup = backup_for_home(home.path());
        assert_eq!(fs::read_to_string(backup.join("sparsh.spar")).unwrap(), "var a: int = 1;\n");
        assert_eq!(fs::read_to_string(backup.join("lib/util.spar")).unwrap(), "var u: int = 2;\n");
        assert_eq!(fs::read_to_string(backup.join("notes.txt")).unwrap(), "keep me\n");
        assert!(!backup.join("spar.package.spar").exists(), "backup predates the manifest");
    }

    #[test]
    fn migration_is_idempotent() {
        let home = flat_home();
        let store = test_store(home.path());
        prepare(home.path(), &store).unwrap();
        assert_eq!(prepare(home.path(), &store).unwrap(), Prepared::AlreadyPackage);
    }

    #[test]
    fn existing_backup_blocks_migration_and_keeps_the_flat_layout() {
        let home = flat_home();
        fs::create_dir(backup_for_home(home.path())).unwrap();
        let outcome = prepare(home.path(), &test_store(home.path())).unwrap();
        assert_eq!(
            outcome,
            Prepared::LegacyKept { notice: BACKUP_EXISTS_NOTICE.to_string() }
        );
        let root = home.path().join(".sparsh");
        assert!(root.join("sparsh.spar").is_file());
        assert!(!root.join("spar.package.spar").exists());
        assert_eq!(entry_for_home(home.path()), root.join("sparsh.spar"));
    }

    #[test]
    fn fresh_install_seeds_the_template_without_a_backup() {
        let home = tempfile::tempdir().unwrap();
        let outcome = prepare(home.path(), &test_store(home.path())).unwrap();
        assert_eq!(outcome, Prepared::Seeded);
        let root = home.path().join(".sparsh");
        assert!(root.join("spar.package.spar").is_file());
        assert!(root.join("spar.package.lock.spar").is_file());
        assert!(fs::read_to_string(root.join("src/config.spar")).unwrap().contains("pkg add"));
        assert!(!backup_for_home(home.path()).exists());
        assert_eq!(entry_for_home(home.path()), root.join("src/config.spar"));
    }

    #[test]
    fn seeding_never_overwrites_an_existing_entry_file() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join(".sparsh/src/config.spar"), "var mine: int = 9;\n");
        prepare(home.path(), &test_store(home.path())).unwrap();
        assert_eq!(
            fs::read_to_string(home.path().join(".sparsh/src/config.spar")).unwrap(),
            "var mine: int = 9;\n"
        );
    }

    #[test]
    fn failure_restores_the_original_tree() {
        let home = flat_home();
        // A directory named `src` that is actually a file makes `create_dir_all(src)` fail.
        write(&home.path().join(".sparsh/src"), "i am a file\n");
        let error = prepare(home.path(), &test_store(home.path())).unwrap_err();
        assert!(error.to_string().contains("cannot migrate ~/.sparsh"), "{error}");
        let root = home.path().join(".sparsh");
        assert_eq!(fs::read_to_string(root.join("sparsh.spar")).unwrap(), "var a: int = 1;\n");
        assert!(root.join("functions.spar").is_file());
        assert!(!root.join("spar.package.spar").exists());
        assert!(home.path().join(".sparsh.failed-migration").exists(), "failed tree kept aside");
    }

    #[test]
    fn locator_is_none_without_a_lockfile_and_some_with_one() {
        let home = tempfile::tempdir().unwrap();
        let store = test_store(home.path());
        let root = home.path().join(".sparsh");
        fs::create_dir_all(&root).unwrap();
        assert!(locator_for(&root, &store).unwrap().is_none());
        prepare(home.path(), &store).unwrap();
        assert!(locator_for(&root, &store).unwrap().is_some());
    }

    #[test]
    fn malformed_lockfile_is_reported_with_the_fix() {
        let home = tempfile::tempdir().unwrap();
        let store = test_store(home.path());
        prepare(home.path(), &store).unwrap();
        let root = home.path().join(".sparsh");
        fs::write(root.join("spar.package.lock.spar"), "this is not a lockfile {{{").unwrap();
        let message = locator_for(&root, &store).err().expect("malformed lock is an error");
        assert!(message.contains("spar.package.lock.spar"), "{message}");
        assert!(message.contains("pkg install"), "{message}");
    }
}
```
Add `#[derive(Debug, PartialEq, Eq)]` on `Prepared` in Step 3. Also add `mod config_home;` to `lib.rs` now so the test file compiles.

Run: `cd sparsh && cargo test -p sparsh-core config_home 2>&1 | tail -15` → Expected: FAIL (types/functions missing).

- [ ] **Step 3: Implement `config_home.rs`**

Above the test module:

```rust
//! The `~/.sparsh` directory as a Spar package: layout, one-time migration
//! from the old flat layout, fresh-install seeding, and the package locator
//! used for `import pkg`.

use std::path::{Path, PathBuf};

use spar::package::{
    commands, GitCommandProvider, Lockfile, ModuleLocator, NetworkPolicy, PackageKind,
    PackageStore, StorePaths, PACKAGE_LOCK_FILE, PACKAGE_MANIFEST_FILE,
};

use crate::environment::EnvironmentService;

pub(crate) const BACKUP_EXISTS_NOTICE: &str =
    "~/.sparsh.bak exists; move it and restart to migrate";
const TEMPLATE: &str = include_str!("../templates/config.spar");
const PACKAGE_NAME: &str = "sparsh-config";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Prepared {
    AlreadyPackage,
    Seeded,
    Migrated,
    LegacyKept { notice: String },
}

#[derive(Debug)]
pub(crate) struct PrepareError {
    pub step: &'static str,
    pub message: String,
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "cannot migrate ~/.sparsh ({}): {}", self.step, self.message)
    }
}

impl std::error::Error for PrepareError {}

fn step_error(step: &'static str, error: impl std::fmt::Display) -> PrepareError {
    PrepareError {
        step,
        message: error.to_string(),
    }
}

pub(crate) fn root_for_home(home: &Path) -> PathBuf {
    home.join(".sparsh")
}

pub(crate) fn backup_for_home(home: &Path) -> PathBuf {
    home.join(".sparsh.bak")
}

pub(crate) fn entry_for_home(home: &Path) -> PathBuf {
    let root = root_for_home(home);
    let entry = root.join("src/config.spar");
    let legacy = root.join("sparsh.spar");
    if !entry.is_file() && legacy.is_file() {
        legacy
    } else {
        entry
    }
}

pub(crate) fn store_for(environment: &EnvironmentService) -> PackageStore {
    let home = environment
        .get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let data = environment
        .get("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let cache = environment
        .get("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    PackageStore::new(StorePaths::new(data, cache))
}

/// The store a session whose `HOME` is `home` (and which has no `XDG_*`
/// overrides) will use; lets tests build the same store `store_for` derives.
#[cfg(test)]
pub(crate) fn store_for_paths_for_tests(home: &Path) -> PackageStore {
    PackageStore::new(StorePaths::new(home.join(".local/share"), home.join(".cache")))
}

pub(crate) fn locator_for(
    root: &Path,
    store: &PackageStore,
) -> Result<Option<ModuleLocator>, String> {
    let lock_path = root.join(PACKAGE_LOCK_FILE);
    if !lock_path.is_file() {
        return Ok(None);
    }
    let lockfile = Lockfile::read(&lock_path)
        .map_err(|error| format!("{}: {error}; run: pkg install", lock_path.display()))?;
    Ok(Some(ModuleLocator::for_root(lockfile, store.clone())))
}

pub(crate) fn prepare(home: &Path, store: &PackageStore) -> Result<Prepared, PrepareError> {
    let root = root_for_home(home);
    if root.join(PACKAGE_MANIFEST_FILE).is_file() {
        return Ok(Prepared::AlreadyPackage);
    }
    if root.join("sparsh.spar").is_file() {
        migrate(home, &root, store)
    } else {
        seed(&root, store)?;
        Ok(Prepared::Seeded)
    }
}

fn migrate(home: &Path, root: &Path, store: &PackageStore) -> Result<Prepared, PrepareError> {
    let backup = backup_for_home(home);
    if backup.exists() {
        return Ok(Prepared::LegacyKept {
            notice: BACKUP_EXISTS_NOTICE.to_string(),
        });
    }
    if let Err(error) = copy_dir_recursive(root, &backup) {
        let _ = std::fs::remove_dir_all(&backup);
        return Err(step_error("backup", error));
    }
    match migrate_in_place(root, store) {
        Ok(()) => Ok(Prepared::Migrated),
        Err(error) => {
            restore(home, root, &backup)?;
            Err(error)
        }
    }
}

fn migrate_in_place(root: &Path, store: &PackageStore) -> Result<(), PrepareError> {
    let src = root.join("src");
    std::fs::create_dir_all(&src).map_err(|error| step_error("create src/", error))?;
    let mut moves = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| step_error("read directory", error))? {
        let entry = entry.map_err(|error| step_error("read directory", error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "src" || name.starts_with('.') || name == PACKAGE_MANIFEST_FILE || name == PACKAGE_LOCK_FILE {
            continue;
        }
        let path = entry.path();
        let movable = if path.is_file() {
            path.extension().is_some_and(|extension| extension == "spar")
        } else {
            path.is_dir() && contains_spar(&path)
        };
        if movable {
            let target = if name == "sparsh.spar" {
                src.join("config.spar")
            } else {
                src.join(&name)
            };
            moves.push((path, target));
        }
    }
    for (from, to) in moves {
        if to.exists() {
            return Err(step_error(
                "move files",
                format!("{} already exists", to.display()),
            ));
        }
        std::fs::rename(&from, &to).map_err(|error| step_error("move files", error))?;
    }
    write_manifest_and_lock(root, store)
}

fn seed(root: &Path, store: &PackageStore) -> Result<(), PrepareError> {
    let entry = root.join("src/config.spar");
    std::fs::create_dir_all(root.join("src")).map_err(|error| step_error("create src/", error))?;
    if !entry.exists() {
        std::fs::write(&entry, TEMPLATE).map_err(|error| step_error("write config.spar", error))?;
    }
    write_manifest_and_lock(root, store)
}

fn write_manifest_and_lock(root: &Path, store: &PackageStore) -> Result<(), PrepareError> {
    commands::init(root, PACKAGE_NAME, PackageKind::Config)
        .map_err(|error| step_error("write manifest", error))?;
    commands::install(
        root,
        &GitCommandProvider::default(),
        NetworkPolicy::Offline,
        store,
    )
    .map_err(|error| step_error("write lockfile", error))?;
    Ok(())
}

fn restore(home: &Path, root: &Path, backup: &Path) -> Result<(), PrepareError> {
    let aside = home.join(".sparsh.failed-migration");
    if aside.exists() {
        return Err(step_error(
            "restore",
            format!("{} already exists; original kept in {}", aside.display(), backup.display()),
        ));
    }
    std::fs::rename(root, &aside).map_err(|error| step_error("restore", error))?;
    copy_dir_recursive(backup, root).map_err(|error| step_error("restore", error))
}

fn contains_spar(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            contains_spar(&path)
        } else {
            path.extension().is_some_and(|extension| extension == "spar")
        }
    })
}

fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir_recursive(&source, &target)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}
```
Verify the names used from `spar::package` exist as public re-exports: `PACKAGE_MANIFEST_FILE` and `PACKAGE_LOCK_FILE` (both re-exported in `spar/src/package/mod.rs`), `commands::init/install` are `pub`, `PackageStore: Clone` (the test fixture in `package_imports.rs` calls `store.clone()`). If `PackageKind` lacks `PartialEq`/`Debug` the `assert_eq!` on `manifest.kind` in the test fails to compile; in that case compare `manifest.kind.as_str()` to `"config"` instead.

Note: `init` only writes the stub entry when `src/config.spar` is missing, so the entry moved/seeded above is never overwritten. `write_manifest_and_lock` for the migrate case runs `init`, which refuses if a manifest exists; `prepare` only calls it when none exists.

- [ ] **Step 4: Run tests**

Run: `cd sparsh && cargo test -p sparsh-core config_home 2>&1 | grep -E "test |test result"`
Expected: all 9 PASS.

- [ ] **Step 5: Commit**

```bash
cd sparsh && git add crates/sparsh-core/src/config_home.rs crates/sparsh-core/src/lib.rs crates/sparsh-core/templates/config.spar crates/sparsh-core/Cargo.toml
git commit -m "feat: config_home module for the ~/.sparsh Spar package (layout, migration, seeding)" -- crates/sparsh-core/src/config_home.rs crates/sparsh-core/src/lib.rs crates/sparsh-core/templates/config.spar crates/sparsh-core/Cargo.toml
```

---

### Task 3: Wire config loading, notices, and imports into the session

**Files:**
- Modify: `sparsh/crates/sparsh-core/src/config.rs` (`config_path` ~line 231; test at ~572)
- Modify: `sparsh/crates/sparsh-core/src/session.rs` (`ShellSession` struct + `try_new` ~231; `reload_config` ~726; tests)
- Modify: `sparsh/src/main.rs` (`load_config_or_report` ~line 54)
- Test: `sparsh/crates/sparsh-core/src/session.rs` test module

**Interfaces:**
- Consumes: `config_home::{prepare, Prepared, entry_for_home, root_for_home, store_for, locator_for}`; `spar::Engine::{with_locator, with_package_command}`.
- Produces: `ShellSession::take_config_notices(&mut self) -> Vec<String>`; `config::config_path(&EnvironmentService) -> Option<PathBuf>` now returns `entry_for_home(home)`.

- [ ] **Step 1: Update the config-path test and write failing session tests**

In `config.rs` replace `config_path_is_exactly_home_dot_sparsh_sparsh_spar` with:

```rust
    #[test]
    fn config_path_is_home_dot_sparsh_src_config_spar_by_default() {
        let environment = EnvironmentService::from_pairs([
            ("XDG_CONFIG_HOME", "/tmp/xdg"),
            ("HOME", "/home/test"),
        ]);
        assert_eq!(
            config_path(&environment),
            Some(PathBuf::from("/home/test/.sparsh/src/config.spar"))
        );

        let empty = EnvironmentService::from_pairs(std::iter::empty::<(&str, &str)>());
        assert_eq!(config_path(&empty), None);
    }
```

In `session.rs` tests (same module as `reload_with_a_broken_prompt_slot_succeeds_and_reports_issues`, reuse `PROCESS_STATE`, `CwdGuard`) add a fixture helper and tests:

```rust
    fn write_tools_package(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("spar.package.spar"),
            "struct Package: SparPackage {\n    name = \"my-tools\";\n    version = \"1.0.0\";\n    kind = \"library\";\n};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.spar"),
            "function dismantler() -> int { return 42; };\n",
        )
        .unwrap();
    }

    /// A HOME with a seeded config package that depends on `my-tools` (a `path:` dependency).
    fn home_with_tools_dependency(config_source: &str) -> (tempfile::TempDir, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write_tools_package(tools.path());
        let store = crate::config_home::store_for_paths_for_tests(home.path());
        crate::config_home::prepare(home.path(), &store).unwrap();
        let root = home.path().join(".sparsh");
        spar::package::commands::add(
            &root,
            "my-tools",
            &format!("path:{}", tools.path().display()),
            &spar::package::GitCommandProvider::default(),
            spar::package::NetworkPolicy::Offline,
            &store,
        )
        .unwrap();
        std::fs::write(root.join("src/config.spar"), config_source).unwrap();
        (home, tools)
    }

    fn session_with_home(home: &std::path::Path) -> ShellSession {
        let mut session = ShellSession::new();
        session
            .submit(&format!("export HOME={}", home.display()))
            .unwrap();
        session
    }

    #[test]
    fn startup_migrates_a_flat_config_and_keeps_it_working() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".sparsh")).unwrap();
        std::fs::write(
            home.path().join(".sparsh/sparsh.spar"),
            format!(
                "{}\nstruct Config: SparshConfig {{\n    keybindings = [{{ key: \"ctrl+l\"; action: \"clearScreen\"; }}];\n}};\n",
                include_str!("../../../examples/sparsh-types.spar")
            ),
        )
        .unwrap();
        let mut session = session_with_home(home.path());

        session.reload_config().unwrap();

        assert_eq!(session.keybindings().len(), 1);
        assert!(home.path().join(".sparsh/spar.package.spar").is_file());
        assert!(home.path().join(".sparsh/src/config.spar").is_file());
        assert!(home.path().join(".sparsh.bak/sparsh.spar").is_file());
    }

    #[test]
    fn config_can_import_a_path_dependency() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency(
            "import pkg { dismantler } from \"my-tools\";\nvar answer: int = dismantler();\n",
        );
        let mut session = session_with_home(home.path());

        session.reload_config().unwrap();

        let result = session.submit_spar("answer").unwrap();
        assert!(
            matches!(result, ShellResult::Value(spar::ConfigValue::Int(42))),
            "{result:?}"
        );
    }

    #[test]
    fn prompt_can_import_a_dependency_and_call_it() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        let mut session = session_with_home(home.path());
        session.reload_config().unwrap();

        session
            .submit_spar("import pkg { dismantler } from \"my-tools\";")
            .unwrap();
        let result = session.submit_spar("dismantler()").unwrap();

        assert!(
            matches!(result, ShellResult::Value(spar::ConfigValue::Int(42))),
            "{result:?}"
        );
    }

    #[test]
    fn script_can_import_a_dependency() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        let mut session = session_with_home(home.path());
        session.reload_config().unwrap();

        let result = session
            .submit_script("import pkg { dismantler } from \"my-tools\";\nvar n: int = dismantler();\n")
            .unwrap();

        assert!(!matches!(result, ShellResult::CommandStatus { status, .. } if status != 0), "{result:?}");
    }

    #[test]
    fn unknown_dependency_alias_points_at_pkg_add() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        let mut session = session_with_home(home.path());
        session.reload_config().unwrap();

        let error = session
            .submit_spar("import pkg { x } from \"nope\";")
            .expect_err("unknown alias must fail");

        let text = error.to_string();
        assert!(text.contains("pkg add nope"), "{text}");
    }

    #[test]
    fn existing_backup_keeps_the_flat_config_and_reports_a_notice() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".sparsh")).unwrap();
        std::fs::create_dir(home.path().join(".sparsh.bak")).unwrap();
        std::fs::write(home.path().join(".sparsh/sparsh.spar"), "var flat: int = 5;\n").unwrap();
        let mut session = session_with_home(home.path());

        session.reload_config().unwrap();

        assert_eq!(
            session.take_config_notices(),
            vec!["~/.sparsh.bak exists; move it and restart to migrate".to_string()]
        );
        let result = session.submit_spar("flat").unwrap();
        assert!(matches!(result, ShellResult::Value(spar::ConfigValue::Int(5))), "{result:?}");
    }

    #[test]
    fn malformed_lockfile_fails_reload_with_a_pkg_install_hint() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let (home, _tools) = home_with_tools_dependency("// empty\n");
        std::fs::write(home.path().join(".sparsh/spar.package.lock.spar"), "not a lock {{{").unwrap();
        let mut session = session_with_home(home.path());

        let error = session.reload_config().expect_err("bad lock must fail the reload");

        assert!(error.to_string().contains("pkg install"), "{error}");
    }
```
The helper `store_for_paths_for_tests(home: &Path) -> PackageStore` is defined in `config_home.rs` (Task 2); it matches what `store_for` derives from a session whose `HOME` is `home` with no `XDG_*` overrides — if the developer machine exports `XDG_DATA_HOME`/`XDG_CACHE_HOME`, `unset` them at the top of each test with `session.submit("unset XDG_DATA_HOME XDG_CACHE_HOME")` so both sides agree).

Run: `cd sparsh && cargo test -p sparsh-core startup_migrates config_can_import prompt_can_import script_can_import unknown_dependency existing_backup malformed_lockfile config_path 2>&1 | tail -20` → Expected: FAIL.

- [ ] **Step 2: Implement `config_path`**

`config.rs`:
```rust
pub(crate) fn config_path(environment: &EnvironmentService) -> Option<PathBuf> {
    environment
        .get("HOME")
        .map(PathBuf::from)
        .map(|home| crate::config_home::entry_for_home(&home))
}
```
Also update the doc comment above `evaluate_source_in_session` (mentions `~/.sparsh/sparsh-types.spar`) to `~/.sparsh/src/`.

- [ ] **Step 3: Implement session wiring**

In `session.rs` add a field `config_notices: Vec<String>` to `ShellSession` (initialize `Vec::new()` in `try_new`) and:

```rust
    pub fn take_config_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.config_notices)
    }
```
Replace the body of `reload_config` up to (and including) the `let mut candidate = ...` line with:

```rust
    pub fn reload_config(&mut self) -> Result<(), ShellError> {
        self.config_notices.clear();
        let store = crate::config_home::store_for(&self.services.environment);
        let mut package_root: Option<PathBuf> = None;
        if let Some(home) = self.services.environment.get("HOME").map(PathBuf::from) {
            match crate::config_home::prepare(&home, &store) {
                Ok(crate::config_home::Prepared::LegacyKept { notice }) => {
                    self.config_notices.push(notice);
                }
                Ok(_) => package_root = Some(crate::config_home::root_for_home(&home)),
                Err(error) => self.config_notices.push(error.to_string()),
            }
        }
        let (source, base_dir) = match crate::config::config_path(&self.services.environment) {
            Some(path) => {
                let source = crate::config::read_source(&path).map_err(ShellError::Config)?;
                let base = path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf();
                (source, base)
            }
            None => (
                String::new(),
                self.services.directories.current().to_path_buf(),
            ),
        };
        let mut engine = spar::Engine::default()
            .with_base_dir(base_dir)
            .with_package_command("pkg");
        if let Some(root) = &package_root {
            match crate::config_home::locator_for(root, &store) {
                Ok(Some(locator)) => engine = engine.with_locator(locator),
                Ok(None) => {}
                Err(message) => {
                    return Err(ShellError::Config(crate::ConfigLoadError::Invalid(message)))
                }
            }
        }
        let mut candidate = engine.session();
```
Leave the rest of the function (evaluate config, replay `interactive_source`, apply, swap) unchanged. Add `use std::path::PathBuf;` if not already imported in `session.rs`.

In `sparsh/src/main.rs`:
```rust
fn load_config_or_report(session: &mut ShellSession, err: &mut impl Write) {
    let result = session.reload_config();
    for notice in session.take_config_notices() {
        let _ = writeln!(err, "sparsh: {notice}");
    }
    if let Err(error) = result {
        let _ = render_error(&error, None, &Theme::plain(), err);
    }
}
```
Also update the `reload` builtin description in `builtin.rs` (~line 397) to `"Reload ~/.sparsh/src/config.spar transactionally"`.

- [ ] **Step 4: Run tests**

Run: `cd sparsh && cargo test --workspace 2>&1 | grep -E "test result|FAILED|panicked" | head -20`
Expected: PASS. Existing tests that write `.sparsh/sparsh.spar` (flat) now exercise auto-migration in their temp HOME and must still pass; if one asserts on the flat path afterwards, update it to `src/config.spar`.

- [ ] **Step 5: Commit**

```bash
cd sparsh && git add crates/sparsh-core/src/config.rs crates/sparsh-core/src/config_home.rs crates/sparsh-core/src/session.rs crates/sparsh-core/src/builtin.rs src/main.rs
git commit -m "feat: load ~/.sparsh as a Spar package with dependency imports" -- crates/sparsh-core/src/config.rs crates/sparsh-core/src/config_home.rs crates/sparsh-core/src/session.rs crates/sparsh-core/src/builtin.rs src/main.rs
```

---

### Task 4: The `pkg` builtin

**Files:**
- Create: `sparsh/crates/sparsh-core/src/builtin/pkg.rs`
- Modify: `sparsh/crates/sparsh-core/src/builtin.rs` (`mod pkg;` near line 8, registration after `reload` ~line 403, expected-names test ~line 968)
- Test: inside `pkg.rs` and `session.rs` tests

**Interfaces:**
- Consumes: `config_home::{root_for_home, store_for}`; `builtin::{error, success, usage_error, BuiltinContext, BuiltinRegistry, BuiltinResult}`.
- Produces: builtin `pkg`; `pub(super) fn run(args: &[String], root: &Path, store: &PackageStore, provider: &dyn PackageProvider) -> Result<PkgOutcome, BuiltinError>` with `pub(super) struct PkgOutcome { pub text: String, pub changed: bool }`.

- [ ] **Step 1: Write failing tests**

`pkg.rs` test module (imports `super::*`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spar::package::{GitCommandProvider, StorePaths};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, PackageStore) {
        let home = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tools.path().join("src")).unwrap();
        std::fs::write(
            tools.path().join("spar.package.spar"),
            "struct Package: SparPackage {\n    name = \"my-tools\";\n    version = \"1.0.0\";\n    kind = \"library\";\n};\n",
        )
        .unwrap();
        std::fs::write(tools.path().join("src/lib.spar"), "function dismantler() -> int { return 42; };\n").unwrap();
        let store = PackageStore::new(StorePaths::new(home.path().join("data"), home.path().join("cache")));
        crate::config_home::prepare(home.path(), &store).unwrap();
        (home, tools, store)
    }

    fn run_ok(root: &Path, store: &PackageStore, values: &[&str]) -> PkgOutcome {
        run(&args(values), root, store, &GitCommandProvider::default()).unwrap()
    }

    #[test]
    fn add_writes_manifest_and_lock_and_requests_reload() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        let outcome = run(&args(&["add", "my-tools", &request]), &root, &store, &GitCommandProvider::default()).unwrap();
        assert!(outcome.changed);
        assert!(outcome.text.contains("my-tools"), "{}", outcome.text);
        let manifest = std::fs::read_to_string(root.join("spar.package.spar")).unwrap();
        assert!(manifest.contains("my-tools"), "{manifest}");
        let tree = run_ok(&root, &store, &["tree"]);
        assert!(!tree.changed);
        assert!(tree.text.contains("my-tools"), "{}", tree.text);
    }

    #[test]
    fn remove_drops_the_dependency() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        run_ok(&root, &store, &["add", "my-tools", &request]);
        let outcome = run_ok(&root, &store, &["remove", "my-tools"]);
        assert!(outcome.changed);
        assert!(!run_ok(&root, &store, &["tree"]).text.contains("my-tools"));
    }

    #[test]
    fn install_offline_succeeds_for_a_locked_path_dependency() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        run_ok(&root, &store, &["add", "my-tools", &request]);
        let outcome = run_ok(&root, &store, &["install", "--offline"]);
        assert!(outcome.changed);
    }

    #[test]
    fn update_reresolves() {
        let (home, tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let request = format!("path:{}", tools.path().display());
        run_ok(&root, &store, &["add", "my-tools", &request]);
        assert!(run_ok(&root, &store, &["update"]).changed);
        assert!(run_ok(&root, &store, &["update", "my-tools"]).changed);
    }

    #[test]
    fn add_with_a_bad_request_changes_nothing() {
        let (home, _tools, store) = fixture();
        let root = home.path().join(".sparsh");
        let before = std::fs::read_to_string(root.join("spar.package.spar")).unwrap();
        let error = run(&args(&["add", "x", "not-a-request"]), &root, &store, &GitCommandProvider::default()).unwrap_err();
        assert_eq!(error.status, 1);
        assert_eq!(std::fs::read_to_string(root.join("spar.package.spar")).unwrap(), before);
    }

    #[test]
    fn usage_errors() {
        let (home, _tools, store) = fixture();
        let root = home.path().join(".sparsh");
        for bad in [vec![], vec!["add", "only-alias"], vec!["remove"], vec!["frobnicate"], vec!["install", "--nope"]] {
            let error = run(&args(&bad), &root, &store, &GitCommandProvider::default()).unwrap_err();
            assert_eq!(error.status, 2, "{bad:?}");
            assert!(error.message.starts_with("usage: pkg"), "{}", error.message);
        }
    }
}
```
Add to `session.rs` tests:

```rust
    #[test]
    fn pkg_add_makes_the_dependency_importable_without_restart() {
        let _lock = PROCESS_STATE.lock().unwrap();
        let _cwd = CwdGuard::capture();
        let home = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        write_tools_package(tools.path());
        let mut session = session_with_home(home.path());
        session.submit("unset XDG_DATA_HOME XDG_CACHE_HOME").unwrap();
        session.reload_config().unwrap();

        session
            .submit(&format!("pkg add my-tools path:{}", tools.path().display()))
            .unwrap();
        session.submit_spar("import pkg { dismantler } from \"my-tools\";").unwrap();
        let result = session.submit_spar("dismantler()").unwrap();

        assert!(matches!(result, ShellResult::Value(spar::ConfigValue::Int(42))), "{result:?}");
    }
```
Run: `cd sparsh && cargo test -p sparsh-core pkg 2>&1 | tail -15` → Expected: FAIL (module missing).

- [ ] **Step 2: Implement `pkg.rs`**

```rust
use std::path::{Path, PathBuf};

use spar::package::{commands, GitCommandProvider, NetworkPolicy, PackageProvider, PackageStore};

use super::{error, success, usage_error, BuiltinContext, BuiltinError, BuiltinRegistry, BuiltinResult};

const USAGE: &str =
    "pkg add <alias> <request> | remove <alias> | install [--offline] | update [alias] | tree";

pub(super) struct PkgOutcome {
    pub text: String,
    pub changed: bool,
}

pub(super) fn pkg(
    args: &[String],
    context: &mut BuiltinContext<'_>,
    _: &BuiltinRegistry,
) -> BuiltinResult {
    let Some(home) = context.services.environment.get("HOME").map(PathBuf::from) else {
        return Err(error("pkg: HOME is not set"));
    };
    let root = crate::config_home::root_for_home(&home);
    let store = crate::config_home::store_for(&context.services.environment);
    let outcome = run(args, &root, &store, &GitCommandProvider::default())?;
    if outcome.changed {
        context.requested_reload_config = true;
    }
    Ok(success(Some(outcome.text)))
}

pub(super) fn run(
    args: &[String],
    root: &Path,
    store: &PackageStore,
    provider: &dyn PackageProvider,
) -> Result<PkgOutcome, BuiltinError> {
    let package_error = |error: spar::package::PackageError| self::error(format!("pkg: {error}"));
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["add", alias, request] => {
            let lock = commands::add(root, alias, request, provider, NetworkPolicy::Allow, store)
                .map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!("added '{alias}' — {} package(s) locked\n", lock.packages.len()),
                changed: true,
            })
        }
        ["remove", alias] => {
            let lock = commands::remove(root, alias, provider, NetworkPolicy::Allow, store)
                .map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!("removed '{alias}' — {} package(s) locked\n", lock.packages.len()),
                changed: true,
            })
        }
        ["install"] | ["install", "--offline"] => {
            let network = if words.len() == 2 {
                NetworkPolicy::Offline
            } else {
                NetworkPolicy::Allow
            };
            let lock = commands::install(root, provider, network, store).map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!("installed — {} package(s) locked\n", lock.packages.len()),
                changed: true,
            })
        }
        ["update"] | ["update", _] => {
            let alias = words.get(1).copied();
            let lock = commands::update(root, alias, provider, store).map_err(package_error)?;
            Ok(PkgOutcome {
                text: format!("updated — {} package(s) locked\n", lock.packages.len()),
                changed: true,
            })
        }
        ["tree"] => {
            let tree = commands::tree(root).map_err(package_error)?;
            let text = if tree.is_empty() {
                "no dependencies\n".to_string()
            } else {
                tree
            };
            Ok(PkgOutcome { text, changed: false })
        }
        _ => Err(usage_error(USAGE)),
    }
}
```
If `BuiltinError`/`BuiltinResult` are not `pub(super)`-visible from the `builtin` submodule, import them the same way `history.rs` does (`use super::{...}`); `BuiltinError` is declared `pub` in `builtin.rs`. `commands::update` takes no `NetworkPolicy` (it always allows network) — the call above matches its signature `(dir, alias, provider, store)`.

Registration in `builtin.rs`: add `mod pkg;` beside `mod history;`, then after the `reload` entry:

```rust
                builtin!(
                    "pkg",
                    "Manage dependencies of the ~/.sparsh config package",
                    "pkg add <alias> <request> | remove <alias> | install [--offline] | update [alias] | tree",
                    "package",
                    true,
                    true,
                    pkg::pkg
                ),
```
and add `"pkg",` to the `expected` list in the registry test (~line 968).

- [ ] **Step 3: Run tests**

Run: `cd sparsh && cargo test --workspace 2>&1 | grep -E "test result|FAILED|panicked" | head -20`
Expected: PASS. If a help/completion snapshot test lists builtins by category and now differs, update the snapshot to include `pkg`.

- [ ] **Step 4: Commit**

```bash
cd sparsh && git add crates/sparsh-core/src/builtin/pkg.rs crates/sparsh-core/src/builtin.rs crates/sparsh-core/src/session.rs
git commit -m "feat: pkg builtin for managing config package dependencies" -- crates/sparsh-core/src/builtin/pkg.rs crates/sparsh-core/src/builtin.rs crates/sparsh-core/src/session.rs
```

---

### Task 5: Docs, template refresh, real-world dry run, install

**Files:**
- Modify: `sparsh/README.md`, `sparsh/examples/*.spar` if they reference `~/.sparsh/sparsh.spar`
- Modify: `sparsh/crates/sparsh-core/config-package/` only if it documents the old path (`grep -rn "sparsh.spar" sparsh --include='*.md' --include='*.spar'`)

- [ ] **Step 1: Update docs**

Search: `cd sparsh && grep -rn "\.sparsh/sparsh.spar\|sparsh\.spar" README.md examples crates/sparsh-core/config-package 2>/dev/null`. Replace every `~/.sparsh/sparsh.spar` with `~/.sparsh/src/config.spar`. Add a "Config package and dependencies" README section covering: the layout, `pkg add/remove/install/update/tree`, the three request formats, `import pkg { name } from "alias"` in config/prompt/scripts, the one-time migration and `~/.sparsh.bak`, and that startup never touches the network.

- [ ] **Step 2: Dry-run the migration against a copy of the real config (never the real directory)**

```bash
tmp=$(mktemp -d) && cp -r ~/.sparsh "$tmp/.sparsh" && cd /home/occ/Projects/Rust/occ_lang/sparsh \
  && cargo build --release 2>&1 | tail -2 \
  && HOME="$tmp" ./target/release/sparsh -c 'echo migrated' \
  && ls -A "$tmp" "$tmp/.sparsh" "$tmp/.sparsh/src"
```
Expected: prints `migrated`; `$tmp/.sparsh.bak` exists; `$tmp/.sparsh/spar.package.spar` and `spar.package.lock.spar` exist; your `sparsh.spar`, `functions.spar`, `sparsh-types.spar` are under `src/` with `sparsh.spar` renamed `config.spar`; no error output. Then exercise a dependency:

```bash
mkdir -p "$tmp/tools/src" && printf 'struct Package: SparPackage {\n    name = "my-tools";\n    version = "1.0.0";\n    kind = "library";\n};\n' > "$tmp/tools/spar.package.spar" \
  && printf 'function dismantler() -> int { return 42; };\n' > "$tmp/tools/src/lib.spar" \
  && printf 'pkg add my-tools path:%s/tools\nimport pkg { dismantler } from "my-tools";\ndismantler()\nexit\n' "$tmp" | HOME="$tmp" ./target/release/sparsh
```
Expected: output includes `added 'my-tools'` and `42`.

- [ ] **Step 3: Full verification**

```bash
cd /home/occ/Projects/Rust/occ_lang/sparsh && cargo fmt --all && cargo test --workspace 2>&1 | grep -E "test result|FAILED" && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cd ../spar && cargo test 2>&1 | grep -E "test result|FAILED"
```
Expected: all PASS, Clippy clean.

- [ ] **Step 4: Install (per the reinstall rule) and commit**

```bash
cd /home/occ/Projects/Rust/occ_lang && cargo install --path spar --force && cargo install --path sparsh --force
cd sparsh && git add README.md examples && git commit -m "docs: config package layout, pkg builtin and dependency imports" -- README.md examples
```
Do NOT launch the newly installed sparsh against the real `~/.sparsh` as part of the plan; the real migration happens on the user's first launch, after which `~/.sparsh.bak` holds the original.

---

## Self-review notes

- Spec coverage: layout + migration + backup + rollback + idempotence (T2); no-network startup, minimal-session failure handling, `~/.sparsh.bak` notice (T2/T3); imports at config/prompt/scripts + unknown alias hint + missing-store hint (T1/T3); `pkg` builtin with reload (T4); error table rows: backup exists (T2/T3), copy/move failure (T2 `failure_restores_the_original_tree`), malformed manifest/lock (T3 `malformed_lockfile...`), missing store entry (hint via T1 `pkg install`), unknown alias (T3), `pkg add` failure atomic (T4 `add_with_a_bad_request_changes_nothing`), config compile failure unchanged.
- Deviations from the spec, all deliberate: the fresh-install seed is a minimal commented template (`templates/config.spar`) rather than the `config-package/` directory (that directory is the bundled `sparsh` types package, a different thing); the unknown-alias hint reads ``run `pkg add <alias> <source>` ...`` via `package_command` rather than embedding the literal example URL; a malformed manifest surfaces as the existing config-load error (shell keeps its default session) rather than a separate minimal-session mode.
- Manifest problems: `commands::init`/`install` errors during migration roll back; a manifest that later becomes malformed is caught when `Lockfile::read`/import resolution fails and is reported by `reload_config` without aborting the shell.
- Type consistency: `Prepared`, `PrepareError`, `entry_for_home`, `root_for_home`, `store_for`, `locator_for`, `PkgOutcome`, `run`, `take_config_notices` are spelled identically everywhere; `store_for_paths_for_tests` is a `#[cfg(test)]` helper defined in `config_home.rs` in Task 2.
