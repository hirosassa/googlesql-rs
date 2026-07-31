//! End-to-end tests for typed AST access.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{AstNode, Module};

/// Recursively searches the node tree for a node whose source text matches `want`.
fn contains_text(node: &AstNode, sql: &str, want: &str) -> bool {
    if node.text(sql) == Some(want) {
        return true;
    }
    node.children().iter().any(|c| contains_text(c, sql, want))
}

/// Recursively searches the node tree for a node whose kind contains `want`.
fn contains_kind(node: &AstNode, want: &str) -> bool {
    if node.kind().contains(want) {
        return true;
    }
    node.children().iter().any(|c| contains_kind(c, want))
}

/// Collects every node of an exact `kind` into `out` in preorder.
fn collect_kind<'a>(node: &'a AstNode, kind: &str, out: &mut Vec<&'a AstNode>) {
    if node.kind() == kind {
        out.push(node);
    }
    for child in node.children() {
        collect_kind(child, kind, out);
    }
}

#[test]
fn builds_typed_ast_tree() {
    let mut module = Module::new().unwrap();
    let sql = "SELECT 1";
    let parsed = module.parse_statement(sql).unwrap();
    let root = parsed.root();

    assert!(!root.kind().is_empty(), "root kind: {:?}", root.kind());
    assert!(!root.children().is_empty(), "root must have child nodes");
    assert!(
        contains_kind(root, "Query") || contains_kind(root, "Select"),
        "AST must contain a Query/Select-family node"
    );
}

#[test]
fn node_source_text_via_byte_range() {
    let mut module = Module::new().unwrap();
    let sql = "SELECT a, 42 FROM t";
    let parsed = module.parse_statement(sql).unwrap();
    let root = parsed.root();

    // Upper-level container nodes may have no position information (byte_range is None).
    // Leaf nodes carry a byte range and can have their text extracted from the original SQL.
    assert!(
        contains_text(root, sql, "a"),
        "must find a node for identifier a"
    );
    assert!(
        contains_text(root, sql, "42"),
        "must find a node for literal 42"
    );
    assert!(
        contains_text(root, sql, "t"),
        "must find a node for table name t"
    );
}

#[test]
fn identifier_node_exposes_its_unquoted_name() {
    let mut module = Module::new().unwrap();
    // The backtick-quoted identifier's source text includes the backticks,
    // but identifier() returns the canonical unquoted name.
    let sql = "SELECT `my col` FROM t";
    let parsed = module.parse_statement(sql).unwrap();
    let root = parsed.root();

    let mut idents = Vec::new();
    collect_kind(root, "ASTIdentifier", &mut idents);
    let names: Vec<_> = idents.iter().filter_map(|n| n.identifier()).collect();
    assert!(names.contains(&"my col"), "identifiers: {names:?}");
    assert!(names.contains(&"t"), "identifiers: {names:?}");

    // A non-identifier node (the root statement) carries no identifier value.
    assert_eq!(root.identifier(), None, "non-identifier node has no name");
}
