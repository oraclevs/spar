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

`await` is valid only inside an `async function`. Top-level await is not supported in programs; an interactive shell such as Sparsh accepts it on a line of its own (see below). Promises are created by async calls; v1 has no manual Promise constructor.

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

`panic(message: "...")` is different from an ordinary runtime error: it is unrecoverable, bypasses `try`/`catch`, and aborts execution even when raised by an unawaited task.

`main` may be synchronous or async and may return `void`, `int`, or `shell`. `spar exec` automatically drives an async main to completion, then maps its eventual result in the same way as a synchronous main.

Promises are runtime resources, not configuration data. `spar emit`, Rust deserialization, and WASM serialization reject unresolved Promise values without exposing their internal identity.

## At the interactive prompt

Sparsh treats `await` as a keyword. A line whose expression contains `await` runs inside an implicit async wrapper, waits for the promise, and shows the value instead of `<promise>`:

```
import pkg { get } from "std/http";
get(url: "https://example.com/data.json")         # <promise>
await get(url: "https://example.com/data.json")   # the response
(await get(url: "https://example.com/data.json")).json()
```

This also works in scripts (`sparsh -c`, piped input). The awaited value becomes `_`, so `await get(...)` followed by `_.status` or `_.json()` works.

`await` is not allowed in a declaration (`var r: HttpResponse = await get(...);`). A session re-evaluates its declarations, which would repeat the request, so await the value on its own line instead.

## `std/http` responses

`get`, `post`, `put`, `patch`, `delete` and `request` return `Promise<HttpResponse>`:

| field | type | meaning |
| --- | --- | --- |
| `status` | `int` | HTTP status code |
| `body` | `str` | response body as text |
| `contentType` | `str` | the `Content-Type` header, empty when absent |

Methods: `text()`, `json()` (a JSON object as a `Record`), `isSuccess()`, `isClientError()`, `isServerError()`. For a JSON array or other JSON root, parse `text()` with `std/json`.

At the Sparsh prompt an `HttpResponse` is shown as a status line followed by the body. A JSON body is decoded into a table or tree, and anything else (HTML, XML, plain text) is shown as text. The raw body stays available as `_.body`.
