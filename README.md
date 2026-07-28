# googlesql-rs

GoogleSQL (ZetaSQL) の Rust バインディング。

[goccy/googlesql-wasm](https://github.com/goccy/googlesql-wasm) が公開する
prebuilt WebAssembly モジュールを [wasmtime](https://wasmtime.dev/) 上で駆動することで、
巨大な C++ / Bazel ツールチェインを一切必要とせずに GoogleSQL のパーサ機能を利用できます。

## 特徴

- **C++ ビルド不要** — GoogleSQL を WASM 化した成果物を実行するだけ
- **cgo 相当の FFI 不要** — `unsafe` は `forbid`
- ビルド時に `googlesql.wasm` を GitHub Release から自動取得(SHA256 検証つき)

## 使い方

```rust
use googlesql::Module;

fn main() -> Result<(), googlesql::Error> {
    let mut module = Module::new()?;
    let parsed = module.parse_statement("select a,b from t where a>1")?;
    println!("{}", parsed.canonical_sql());
    // SELECT
    //   a,
    //   b
    // FROM
    //   t
    // WHERE
    //   a > 1
    Ok(())
}
```

構文エラーは `Error::GoogleSql` として返ります。

## 現状(MVP)

- ✅ SQL 文のパースと正規化(`parse_statement` → `canonical_sql`)
- ⬜ AST ノードへの型付きアクセス
- ⬜ アナライザ(型推論・名前解決、Catalog コールバック)
- ⬜ フォーマッタ

## ビルド

初回ビルドは `googlesql.wasm`(約 14MB)をダウンロードします。
オフライン環境やローカルの wasm を使う場合は環境変数で上書きできます。

```sh
# ローカルの wasm を使う(ダウンロードをスキップ)
GOOGLESQL_WASM=/path/to/googlesql.wasm cargo build
```

内部アーキテクチャと WASM ホスト ABI の詳細は [`docs/SPIKE.md`](docs/SPIKE.md) を参照してください。

## ライセンス

Apache-2.0(GoogleSQL / ZetaSQL に準拠)。
