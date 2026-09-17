# Async functions and promises

Spar async functions declare their eventual result type. Calling one starts a task and returns a built-in `Promise<T>`; `await` extracts the result.

```spar
async function double(value: int) -> int {
    return value * 2;
};

async function main() -> int {
    var pending: Promise<int> = double(value: 21);
    var answer: int = await pending;
    return answer - 42;
};
```

`await` is valid only inside an `async function`. Top-level await is not supported. Promises are created by async calls; v1 has no manual Promise constructor.

Several calls can be started before awaiting either result:

```spar
var left: Promise<int> = double(value: 10);
var right: Promise<int> = double(value: 20);
var total: int = await left + await right;
```

Each task executes at most once. Repeated awaits reuse its completed value. Awaiting a failed task raises the same Spar error in the awaiting context, so existing `try`/`catch` handles it:

```spar
async function fail() -> int {
    return 1 / 0;
};

async function recover() -> int {
    try {
        return await fail();
    } catch error {
        return error.code;
    }
};
```

`main` may be synchronous or async and may return `void`, `int`, or `shell`. `spar exec` automatically drives an async main to completion, then maps its eventual result in the same way as a synchronous main.

Promises are runtime resources, not configuration data. `spar emit`, Rust deserialization, and WASM serialization reject unresolved Promise values without exposing their internal identity.
