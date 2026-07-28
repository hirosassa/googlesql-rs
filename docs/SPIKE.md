# スパイク結果: googlesql.wasm のホストABI実測

対象アーティファクト: `goccy/googlesql-wasm` release **v0.3.4** の `googlesql.wasm` (13.9MB)
リファレンス実装: 同release の `googlesql_wazero.go`(wazeroホストグルー、340k行)

## 結論: 案A(prebuilt wasm + Rust WASMランタイム)は実現可能。最大リスク(C++ env関数)は解消済み

wazeroグルーが完全なホストABIリファレンスになっており、Rust(wasmtime)へそのまま移植できる。

## Imports(ホストが供給する41関数)

| module | 数 | Rustでの対応 |
|---|---|---|
| `wasi_snapshot_preview1` | 15 | `wasmtime-wasi` が無償提供 |
| `wasmify::callback_invoke` | 1 | ホスト関数を1つ実装(下記コールバック規約) |
| `env`(C++ランタイム: absl/icu/C++例外) | 25 | **全てスタブでよい**(実装不要) |

### env スタブ規約(wazeroと同一に移植)
- 名前に `SemWait`/`sem_wait` を含む → `1`(成功)を返す(wasmはシングルスレッド)
- 名前に `allocate_exception` を含む → `wasm_alloc(size)`(size==0なら64)で確保して返す
- 名前に `__cxa_throw`/`_throw` を含む → trap(巻き戻し不可のためabort)
- それ以外 → 全戻り値に `0` を返すだけ

## Exports(26,323個)

- `wasm_alloc(len) -> ptr` / `wasm_free(ptr)` … アロケータ
- `_initialize` … WASIリアクタ初期化(C++グローバルコンストラクタ実行)。**必ず呼ぶ**
- `wasm_init`(あれば呼ぶ)/ `wasmify_get_type_name`(型名解決ヘルパ)
- `w_<serviceID>_<methodID>` … wasmify RPCメソッド本体(数万個)

## インスタンス化手順(wasmtimeへ移植)

1. WASIをリアクタとして(＝`_start`を呼ばない)リンク。**`/` をマウント**(タイムゾーン/ICUデータ読取のため)
2. ホストモジュール `wasmify` に `callback_invoke(cbID i32, methodID i32, reqPtr i32, reqLen i32) -> i64` を登録
3. ホストモジュール `env` に、wasmが要求する各シンボルを上記スタブ規約で登録
4. インスタンス化 → `wasm_alloc`/`wasm_free` を取得(必須)、`_initialize` を呼ぶ、`wasm_init` があれば呼ぶ

## RPC呼び出し規約 `invoke(serviceID, methodID, reqBytes) -> respBytes`

1. export `w_<serviceID>_<methodID>` を引く(初回のみルックアップ、以降キャッシュ)
2. `req` が非空なら `ptr = wasm_alloc(len)`(0ならOOMエラー)、`memory[ptr..].write(req)`
3. `packed = fn(ptr, len)`(reqが空なら `fn(0,0)`)
4. `respPtr = packed >> 32`、`respLen = packed & 0xFFFFFFFF`
5. `respLen==0` なら応答なし。非0なら `memory[respPtr..respPtr+respLen]` を**コピー**して取り出す
6. `wasm_free(respPtr)` と `wasm_free(reqPtr)` を必ず解放
7. モジュールは**単一インスタンスを排他制御**(wazeroは per-module mutex)。Rustでは `&mut Store` / `Mutex` で直列化する

## コールバック規約(Catalog等のホスト実装を渡す機構)

- ホストは `CallbackHandler`(`handle(methodID, req) -> resp`)を登録し、`callbackID`(i32)を得る
- guestが `callback_invoke(cbID, methodID, reqPtr, reqLen)` を呼ぶ → ホストがcbIDでhandlerを引き、reqを読み、respを`wasm_alloc`して書き、`ptr<<32 | len` を返す
- エラーは protobuf field #15 の string として符号化
- **MVP(パーサのみ)では未使用**(Catalog不要)。後続のAnalyzerで必要

## protobufの扱い

wazeroグルーは **protoc/prostを使わず手書きのwire-format encode/decode**(`pbAppend*` / `pbReader`)。
各メソッドの `(serviceID, methodID)` とリクエスト/レスポンスのfield番号は**生成済みGoグルー内に埋め込まれている**。
→ Rust側は次のいずれか:
  - (推奨・MVP) 必要なメソッドだけ `wazero.go` から `(svc, mid)` とfield定義を読み取り、手書きで最小encode/decode
  - (将来) `googlesql-wasm` の proto 定義を取得し `prost` で網羅生成

## APIモデル: ハンドルベース(オブジェクト指向)

**重要**: APIはメッセージ値をやり取りするのではなく、**wasm内のC++オブジェクトへのポインタ(ハンドル=u64)**をやり取りする。
- リクエストにオブジェクトを渡す時は `pbAppendHandlePtr(field, obj)` でそのポインタを書く
- レスポンスの `readPtrAtField(resp, field)` でハンドル(ポインタ)を受け取り、Go側ラッパー型で包む
- スカラー値(string/bool等)を返すメソッドは `readScalarAtField` で直接値を取れる
- Go側はハンドルに `runtime.SetFinalizer` を張り、GC時にwasm側デストラクタ+`UnregisterCallback`を呼ぶ
  → **Rustでは各ハンドル型に `Drop` を実装し、対応するdestructメソッドを呼ぶ**必要がある
- 親を生かすため `setKeepAlive(parent)` で参照を保持(例: ParserOutput は ParserOptions を生かす)
  → Rustでは所有/ライフタイムかArc参照で親を保持

## MVPの具体的な呼び出しチェーン(実測済み)

`svc0/mid` は `invokeMethod(serviceID, methodID, buf)` の第1,2引数。ParseStatement系は service 0:

| API | svc | mid | req fields | resp |
|---|---|---|---|---|
| `ParseStatement(sql, opts)` | 0 | 10 | f1=string(sql), f2=handle(ParserOptions) | f2=handle(ParserOutput) |
| `ParseType(s, opts)` | 0 | 11 | f1=string, f2=handle | f2=handle |
| `Unparse(root)` | 0 | 12 | f1=handle(ASTNode) | f1=**string**(正規化SQL) |
| `ValidateAnalyzerOptions(opts)` | 0 | 13 | f1=handle | (errのみ) |

### 確定したMVPマッピング(全て実測済み)

| API | svc | mid | req | resp |
|---|---|---|---|---|
| `NewParserOptions()` | 699 | 0 | (なし) | f1=uint64(ParserOptionsハンドル) |
| `ParseStatement(sql, opts)` | 0 | 10 | f1=string(sql), f2=handle(opts, nil可) | f2=handle(ParserOutput) |
| `ParserOutput.Node()` | 700 | 3 | f1=handle(ParserOutput) | f1=handle(ASTNode) |
| `ParserOutput.Statement()` | 700 | 6 | f1=handle(ParserOutput) | f1=handle(ASTStatementNode) |
| `Unparse(node)` | 0 | 12 | f1=handle(ASTNode) | f1=**string**(正規化SQL) |

- パースエラーは応答の field#15(string)に載る → `invokeMethod` 相当がそれを検出して `Result::Err`
- **デストラクタ規約**: 各型が `invoke(その型のserviceID, destructor_mid, handle(ptr))`。
  例: `ParserOptions.free` = `invoke(699, 12, handle)`。
  → Rustでは各ハンドル型の `Drop` でこれを呼ぶ。ParserOutput等の正確なdestructor midは実装時にgrep

### E2Eスモークテスト(最初のTDDゴール)
`Module::new()` → `NewParserOptions()` → `ParseStatement("SELECT 1", opts)` →
`ParserOutput.Node()` → `Unparse(node)` で **正規化SQL文字列** を得る。
文字列で結果検証できるので最終スモークに最適。パース失敗(例 `"SELECT FROM"`)は `Err` を返すこともテスト。

## protobuf wire-format 仕様(手書き実装用・実測済み)

- **tag** = varint(`field << 3 | wireType`)
- **varint** = 標準 LEB128(下位7bitずつ、継続ビット0x80)
- **string / bytes / submessage**(wireType 2) = tag + varint(len) + payload
- **handle**(オブジェクトポインタ)の2形態:
  - *submessageパターン*(引数渡し `pbAppendHandle`): tag(field,2) + varint(inner_len) + `0x08`(inner field1,varint) + varint(ptr)。inner_len = 1 + varintlen(ptr)
  - *直接varintパターン*(コンストラクタ応答): field1 を wireType0 の varint として ptr を直接格納
- **応答からハンドル取得** `readPtrAtField(resp, f)`:
  - f==1 は直接varint/submessage両対応(`readHandlePtr`)
  - f≠1 は該当fieldのsubmessage内 field1 varint
- **エラー**: 応答に field **15**(string, wireType2)があれば GoogleSQL エラー → `Result::Err(Error::GoogleSql(..))`

### MVP呼び出しチェーンで必要なエンコード/デコード
- encode: `append_string(1, sql)`, `append_handle(2, opts_ptr)`, `append_handle(1, handle)`
- decode: 応答 field15 → エラー / handle(直接varint or submessage) / string(field1)

### ハンドル破棄(Drop実装用・実測済み)
- `ParserOptions` 破棄 = `invoke(699, 12, append_handle(1, ptr))`
- `ParserOutput` 破棄 = `invoke(700,  9, append_handle(1, ptr))`
- AST ノードは Arena 所有のため個別破棄不要(ParserOutput 破棄で木ごと解放)

## 実装済み(現時点)

- `src/error.rs` — `Error`(thiserror)
- `src/runtime.rs` — `Module`(wasmtime): `new/alloc/free/write/read/invoke`。WASIリアクタ+envスタブ+callback+`_initialize` 実装済み
- `tests/runtime.rs` — インスタンス化+メモリ往復、実RPC(NewParserOptions)疎通。**2件green・clippyクリーン**

## 次の実装(MVP完成まで)

1. `src/pb.rs` — 上記wire-formatの最小 encode/decode(TDD: 単体テストから)
2. `src/parser.rs` — `Module::parse_statement(sql)`:
   NewParserOptions(699,0) → ParseStatement(0,10) → ParserOutput.Node(700,3) → Unparse(0,12)
   → 正規化SQL文字列。取得したハンドルは破棄(699,12 / 700,9)
3. `tests/parser.rs` — `"SELECT 1"` の parse→unparse が正規化SQLを返す / `"SELECT FROM"` は `Err`

## Rustクレート構成(MVP)

- `src/pb.rs` — 最小protobuf wire encode/decode(append string/handle, read handle/string/field#15 error)
- `src/runtime.rs` — wasmtimeホスト(engine/store/instance, WASIリアクタ, envスタブ, wasmify callback, alloc/free/invoke)
- `src/parser.rs` — ParserOptions/ParserOutput/ASTNode ハンドル型(Drop実装), parse_statement, unparse
- `src/error.rs` — `thiserror` エラー型
- `src/lib.rs` — 公開API

## 参考ファイル(spike/ に取得済み・.gitignore対象)

- `spike/googlesql.wasm`(13.9MB)
- `spike/wazero.go`(10.6MB, 移植リファレンス)
- `spike/parse_wasm.py`(import/export実測スクリプト)
