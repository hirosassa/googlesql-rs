//! AST 型付きアクセスのE2Eテスト。
#![allow(clippy::unwrap_used)]

use googlesql::{AstNode, Module};

/// ノード木を再帰的に探し、ソーステキストが `want` に一致するノードがあるか。
fn contains_text(node: &AstNode, sql: &str, want: &str) -> bool {
    if node.text(sql) == Some(want) {
        return true;
    }
    node.children().iter().any(|c| contains_text(c, sql, want))
}

/// 型名(kind)を再帰的に探す。
fn contains_kind(node: &AstNode, want: &str) -> bool {
    if node.kind().contains(want) {
        return true;
    }
    node.children().iter().any(|c| contains_kind(c, want))
}

#[test]
fn builds_typed_ast_tree() {
    let mut module = Module::new().unwrap();
    let sql = "SELECT 1";
    let parsed = module.parse_statement(sql).unwrap();
    let root = parsed.root();

    assert!(!root.kind().is_empty(), "root の kind: {:?}", root.kind());
    assert!(!root.children().is_empty(), "root に子ノードがあること");
    assert!(
        contains_kind(root, "Query") || contains_kind(root, "Select"),
        "AST に Query/Select 系ノードがあること"
    );
}

#[test]
fn node_source_text_via_byte_range() {
    let mut module = Module::new().unwrap();
    let sql = "SELECT a, 42 FROM t";
    let parsed = module.parse_statement(sql).unwrap();
    let root = parsed.root();

    // 上位のコンテナノードは位置情報を持たない(byte_range が None)ことがある。
    // 葉ノードはバイト範囲を持ち、元 SQL からテキストを取り出せる。
    assert!(contains_text(root, sql, "a"), "識別子 a のノードがあること");
    assert!(
        contains_text(root, sql, "42"),
        "リテラル 42 のノードがあること"
    );
    assert!(
        contains_text(root, sql, "t"),
        "テーブル名 t のノードがあること"
    );
}
