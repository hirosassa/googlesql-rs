# Spike: measuring the host ABI of googlesql.wasm

Target artifact: `googlesql.wasm` (13.9MB) from `goccy/googlesql-wasm` release **v0.3.4**
Reference implementation: `googlesql_wazero.go` (the wazero host glue, ~340k lines) from the same release.

## Conclusion: Option A (prebuilt wasm + a Rust WASM runtime) is feasible; the biggest risk (the C++ `env` functions) is resolved

The wazero glue is a complete host-ABI reference and can be ported directly to Rust (wasmtime).

## Imports (41 functions the host must provide)

| module | count | Rust equivalent |
|---|---|---|
| `wasi_snapshot_preview1` | 15 | provided for free by `wasmtime-wasi` |
| `wasmify::callback_invoke` | 1 | implement one host function (see callback protocol below) |
| `env` (C++ runtime: absl / icu / C++ exceptions) | 25 | **all can be stubs** (no real implementation needed) |

### `env` stub rules (ported verbatim from wazero)
- name contains `SemWait` / `sem_wait` → return `1` (success); the wasm is single-threaded
- name contains `allocate_exception` → allocate via `wasm_alloc(size)` (use 64 if size == 0) and return the pointer
- name contains `__cxa_throw` / `_throw` → trap (cannot unwind, so abort)
- otherwise → just return `0` for every result

## Exports (26,323)

- `wasm_alloc(len) -> ptr` / `wasm_free(ptr)` — the allocator
- `_initialize` — WASI reactor init (runs C++ global constructors). **Must be called.**
- `wasm_init` (call it if present) / `wasmify_get_type_name` (type-name resolution helper)
- `w_<serviceID>_<methodID>` — the wasmify RPC method bodies (tens of thousands)

## Instantiation steps (porting to wasmtime)

1. Link WASI as a reactor (i.e. do **not** call `_start`). **Mount `/`** (to read timezone / ICU data).
2. Register host module `wasmify` with `callback_invoke(cbID i32, methodID i32, reqPtr i32, reqLen i32) -> i64`.
3. Register host module `env`, supplying every symbol the wasm requires using the stub rules above.
4. Instantiate → obtain `wasm_alloc` / `wasm_free` (required), call `_initialize`, and call `wasm_init` if present.

## RPC protocol: `invoke(serviceID, methodID, reqBytes) -> respBytes`

1. Look up export `w_<serviceID>_<methodID>` (resolve once, then cache).
2. If `req` is non-empty: `ptr = wasm_alloc(len)` (OOM error if 0), then `memory[ptr..].write(req)`.
3. `packed = fn(ptr, len)` (call `fn(0, 0)` when `req` is empty).
4. `respPtr = packed >> 32`, `respLen = packed & 0xFFFFFFFF`.
5. If `respLen == 0` there is no response; otherwise **copy** out `memory[respPtr..respPtr+respLen]`.
6. Always free both `wasm_free(respPtr)` and `wasm_free(reqPtr)`.
7. The module is a **single instance guarded for exclusive access** (wazero uses a per-module mutex). In Rust, serialize via `&mut Store` / `Mutex`.

## Callback protocol (the mechanism for passing host implementations such as Catalog)

- The host registers a `CallbackHandler` (`handle(methodID, req) -> resp`) and gets a `callbackID` (i32).
- The guest calls `callback_invoke(cbID, methodID, reqPtr, reqLen)` → the host looks up the handler by cbID, reads `req`, writes `resp` via `wasm_alloc`, and returns `ptr<<32 | len`.
- Errors are encoded as a string in protobuf field #15.
- **Unused in the MVP (parser only)** (no Catalog needed); required later for the Analyzer.

## How protobuf is handled

The wazero glue does **hand-written wire-format encode/decode without protoc/prost** (`pbAppend*` / `pbReader`).
Each method's `(serviceID, methodID)` and the request/response field numbers are **baked into the generated Go glue**.
→ On the Rust side, either:
  - (recommended, MVP) read only the `(svc, mid)` and field layout of the methods you need from `wazero.go`, and hand-write a minimal encode/decode
  - (future) fetch the proto definitions from `googlesql-wasm` and generate exhaustively with `prost`

## API model: handle-based (object-oriented)

**Important**: the API does not pass message values; it passes **pointers to C++ objects living inside the wasm (handles = u64)**.
- To pass an object in a request, write its pointer with `pbAppendHandlePtr(field, obj)`.
- On the response, receive the handle (pointer) with `readPtrAtField(resp, field)` and wrap it in a Go wrapper type.
- Methods returning scalars (string/bool/etc.) yield the value directly via `readScalarAtField`.
- On the Go side, handles get a `runtime.SetFinalizer` that calls the wasm-side destructor + `UnregisterCallback` on GC.
  → **In Rust, implement `Drop` on each handle type and call the corresponding destructor method.**
- To keep the parent alive, a reference is pinned via `setKeepAlive(parent)` (e.g. ParserOutput keeps ParserOptions alive).
  → In Rust, keep the parent alive via ownership/lifetime or an Arc reference.

## Concrete MVP call chain (measured)

`svc`/`mid` are the first two arguments to `invokeMethod(serviceID, methodID, buf)`. The ParseStatement family is service 0:

| API | svc | mid | req fields | resp |
|---|---|---|---|---|
| `ParseStatement(sql, opts)` | 0 | 10 | f1=string(sql), f2=handle(ParserOptions) | f2=handle(ParserOutput) |
| `ParseType(s, opts)` | 0 | 11 | f1=string, f2=handle | f2=handle |
| `Unparse(root)` | 0 | 12 | f1=handle(ASTNode) | f1=**string** (canonical SQL) |
| `ValidateAnalyzerOptions(opts)` | 0 | 13 | f1=handle | (error only) |

### Confirmed MVP mapping (all measured)

| API | svc | mid | req | resp |
|---|---|---|---|---|
| `NewParserOptions()` | 699 | 0 | (none) | f1=uint64 (ParserOptions handle) |
| `ParseStatement(sql, opts)` | 0 | 10 | f1=string(sql), f2=handle(opts, nil allowed) | f2=handle(ParserOutput) |
| `ParserOutput.Node()` | 700 | 3 | f1=handle(ParserOutput) | f1=handle(ASTNode) |
| `ParserOutput.Statement()` | 700 | 6 | f1=handle(ParserOutput) | f1=handle(ASTStatementNode) |
| `Unparse(node)` | 0 | 12 | f1=handle(ASTNode) | f1=**string** (canonical SQL) |

- Parse errors are carried in response field #15 (string) → the `invokeMethod` equivalent detects it and returns `Result::Err`.
- **Destructor rule**: each type does `invoke(serviceID of that type, destructor_mid, handle(ptr))`.
  Example: `ParserOptions.free` = `invoke(699, 12, handle)`.
  → In Rust, call this from each handle type's `Drop`. The exact destructor mids for ParserOutput etc. are grepped during implementation.

### E2E smoke test (first TDD goal)
`Module::new()` → `NewParserOptions()` → `ParseStatement("SELECT 1", opts)` →
`ParserOutput.Node()` → `Unparse(node)` yields the **canonical SQL string**.
A string result is easy to assert on, making it ideal for the final smoke test. Also assert that a parse failure (e.g. `"SELECT FROM"`) returns `Err`.

## protobuf wire-format spec (for the hand-written implementation; measured)

- **tag** = varint(`field << 3 | wireType`)
- **varint** = standard LEB128 (7 bits at a time, continuation bit 0x80)
- **string / bytes / submessage** (wireType 2) = tag + varint(len) + payload
- **handle** (object pointer), two shapes:
  - *submessage shape* (argument passing, `pbAppendHandle`): tag(field,2) + varint(inner_len) + `0x08` (inner field1, varint) + varint(ptr); inner_len = 1 + varintlen(ptr)
  - *direct varint shape* (constructor responses): field1 stores the ptr directly as a wireType-0 varint
- **reading a handle from a response** `readPtrAtField(resp, f)`:
  - f == 1 handles both direct-varint and submessage (`readHandlePtr`)
  - f != 1 reads field1 varint inside the submessage at that field
- **error**: if the response has field **15** (string, wireType 2) it is a GoogleSQL error → `Result::Err(Error::GoogleSql(..))`

### Encode/decode needed for the MVP call chain
- encode: `append_string(1, sql)`, `append_handle(2, opts_ptr)`, `append_handle(1, handle)`
- decode: response field15 → error / handle (direct varint or submessage) / string (field1)

### Handle destruction (for `Drop`; measured)
- `ParserOptions` destroy = `invoke(699, 12, append_handle(1, ptr))`
- `ParserOutput` destroy = `invoke(700,  9, append_handle(1, ptr))`
- AST nodes are arena-owned, so they need no individual destruction (destroying ParserOutput frees the whole tree)

## Implemented (as of this spike)

- `src/error.rs` — `Error` (thiserror)
- `src/runtime.rs` — `Module` (wasmtime): `new/alloc/free/write/read/invoke`. WASI reactor + env stubs + callback + `_initialize` implemented
- `tests/runtime.rs` — instantiation + memory roundtrip, and a live RPC (NewParserOptions). **2 passing, clippy clean**

## Remaining implementation (to complete the MVP)

1. `src/pb.rs` — minimal encode/decode for the wire-format above (TDD: start from unit tests)
2. `src/parser.rs` — `Module::parse_statement(sql)`:
   NewParserOptions(699,0) → ParseStatement(0,10) → ParserOutput.Node(700,3) → Unparse(0,12)
   → canonical SQL string. Destroy the acquired handles (699,12 / 700,9).
3. `tests/parser.rs` — `"SELECT 1"` parse→unparse returns canonical SQL / `"SELECT FROM"` returns `Err`

## Rust crate layout (MVP)

- `src/pb.rs` — minimal protobuf wire encode/decode (append string/handle, read handle/string/field#15 error)
- `src/runtime.rs` — wasmtime host (engine/store/instance, WASI reactor, env stubs, wasmify callback, alloc/free/invoke)
- `src/parser.rs` — ParserOptions/ParserOutput/ASTNode handle types (Drop), parse_statement, unparse
- `src/error.rs` — `thiserror` error type
- `src/lib.rs` — public API
