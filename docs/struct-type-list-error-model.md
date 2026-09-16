# Struct, Type, List, and Error Model

This document records the Spar v1 language checkpoint that completes Phase 4. Phase 5 async work depends on these decisions and has not started.

## Values and reusable shapes

`struct` declares a named concrete configuration value. It replaces named section syntax as the canonical spelling:

```spar
struct Server {
    host: str = "localhost";
    port: int = 8080;
};
```

`type` declares a reusable structural type. Types may have generic parameters:

```spar
type Pair<T, V> {
    left: T;
    right: V;
};
```

Structs are concrete and cannot declare generic parameters. Instantiate a generic type from a typed struct instead:

```spar
struct Example: Pair<str, int> {
    left = "hello";
    right = 42;
};
```

The colon means conformance. Generic arguments are substituted before each field is checked. Missing required fields, extra fields, and wrong field types are compile-time errors. A typed struct normally omits duplicate field annotations because its target type is the source of truth.

An untyped struct remains self-described:

```spar
struct App {
    name: str = "Spar";
    debug: bool = false;
};
```

Anonymous `{ ... }` object values remain available for nested data and are checked against their expected structural type. Spar v1 does not add a new `schema` keyword or richer range, regex, serialization, or custom-validation system.

## Defaults and optional fields

Structural type fields may provide defaults:

```spar
type ServerConfig {
    host: str = "localhost";
    port: int = 8080;
    debug: bool = false;
};

struct Development: ServerConfig {
    debug = true;
};
```

Explicit struct fields override defaults. A required field without a default must be supplied. Optional fields may be omitted.

## Lists

`List<T>` is the canonical built-in generic list type:

```spar
var names: List<str> = ["Obi", "Ada"];

type Container<T> {
    values: List<T>;
};
```

The checker represents `List<T>` with the same recursive type machinery used during generic substitution. Runtime values retain the intrinsic list representation; no runtime generic dispatch or second list implementation is introduced.

Legacy `[T]` list syntax remains accepted and resolves to the same semantic type. The formatter emits `List<T>`.

## Compatibility syntax

Legacy named sections remain accepted during migration:

```spar
[Server] { port: int = 8080; };
```

They use the same AST, checker, evaluator, and `ConfigValue::Section` runtime path as `struct Server`. Legacy `type [Name]` and typed-section arrow syntax also remain accepted. The formatter emits canonical `type Name`, `struct Name`, colon conformance, and `List<T>` syntax.

The compiler currently has no general warning channel shared by parser, CLI, LSP, and embedding APIs. Compatibility syntax therefore remains silent rather than introducing a separate warning subsystem solely for this migration.

## Catch bindings and Spar errors

Catch may bind the raised Spar error value to any identifier:

```spar
try {
    var content: str = readText(path: "./config.txt");
} catch err {
    println(err.message);
}
```

The binding has built-in type `error`. Its stable fields include `message: str` and `kind: str`. The runtime carries failures through the existing centralized `ConfigValue::Error` representation. Async failures must reuse this model when Phase 5 begins.

Use an unbound catch when the error value is not needed:

```spar
try {
    riskyOperation();
} catch {
    recover();
}
```

`catch error` remains valid compatibility syntax because `error` is parsed as an ordinary binding identifier in that position; it is not the type name there.

## Performance boundary

Generic substitutions and typed-struct conformance are resolved during static checking. Runtime struct access uses evaluated field maps and compiled module paths. Field access does not reconstruct generic substitutions or resolve type parameters by source string.
