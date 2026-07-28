//! パーサ機能(parse_statement → 正規化SQL)のE2Eテスト。
#![allow(clippy::unwrap_used)]

use googlesql::{Error, Module};

/// 有効なSQLをパースし、正規化SQL文字列を取り出せること。
#[test]
fn parses_and_canonicalizes_select() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_statement("select 1").unwrap();

    let canonical = parsed.canonical_sql();
    assert!(
        canonical.to_uppercase().contains("SELECT"),
        "正規化SQLに SELECT が含まれること: {canonical:?}"
    );
    assert!(
        canonical.contains('1'),
        "正規化SQLに 1 が含まれること: {canonical:?}"
    );
}

/// 構文エラーのSQLは GoogleSql エラーを返すこと。
#[test]
fn returns_error_for_invalid_sql() {
    let mut module = Module::new().unwrap();
    let err = module.parse_statement("SELECT FROM").unwrap_err();
    assert!(
        matches!(err, Error::GoogleSql(_)),
        "構文エラーは Error::GoogleSql になること: {err:?}"
    );
}
