//! End-to-end tests for parser functionality (parse_statement → canonical SQL).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "test code"
)]

use googlesql::{Error, Module};

/// A valid SQL statement can be parsed and the canonical SQL string extracted.
#[test]
fn parses_and_canonicalizes_select() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_statement("select 1").unwrap();

    let canonical = parsed.canonical_sql();
    assert!(
        canonical.to_uppercase().contains("SELECT"),
        "canonical SQL must contain SELECT: {canonical:?}"
    );
    assert!(
        canonical.contains('1'),
        "canonical SQL must contain 1: {canonical:?}"
    );
}

/// A `QUALIFY` clause parses and round-trips through the canonical SQL.
///
/// `QUALIFY` is gated behind a GoogleSQL language feature; the parser enables
/// the maximum language feature set so the clause is accepted.
#[test]
fn parses_qualify_clause() {
    let mut module = Module::new().unwrap();
    let sql = "SELECT a FROM t QUALIFY ROW_NUMBER() OVER (PARTITION BY b ORDER BY a) = 1";
    let parsed = module.parse_statement(sql).unwrap();

    assert!(
        parsed.canonical_sql().to_uppercase().contains("QUALIFY"),
        "canonical SQL must contain QUALIFY: {:?}",
        parsed.canonical_sql()
    );
}

/// A SQL statement with a syntax error returns a GoogleSql error.
#[test]
fn returns_error_for_invalid_sql() {
    let mut module = Module::new().unwrap();
    let err = module.parse_statement("SELECT FROM").unwrap_err();
    assert!(
        matches!(err, Error::GoogleSql(_)),
        "a syntax error must produce Error::GoogleSql: {err:?}"
    );
}

/// A semicolon-separated script yields one `ParsedStatement` per statement.
#[test]
fn parses_multiple_statements() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_statements("SELECT 1; SELECT 2").unwrap();

    assert!(
        parsed.is_complete(),
        "no error expected: {:?}",
        parsed.error()
    );
    let statements = parsed.statements();
    assert_eq!(statements.len(), 2, "expected two statements");
    assert!(statements[0].canonical_sql().contains('1'));
    assert!(statements[1].canonical_sql().contains('2'));
}

/// A trailing semicolon does not produce an extra empty statement.
#[test]
fn parses_statements_with_a_trailing_semicolon() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_statements("SELECT 1;").unwrap();

    assert!(
        parsed.is_complete(),
        "no error expected: {:?}",
        parsed.error()
    );
    assert_eq!(parsed.statements().len(), 1);
}

/// A syntax error mid-script returns the statements parsed so far plus the error.
#[test]
fn parse_statements_returns_prefix_and_error_on_failure() {
    let mut module = Module::new().unwrap();
    let parsed = module
        .parse_statements("SELECT 1; SELECT FROM; SELECT 3")
        .unwrap();

    // The first statement parses; parsing halts at the syntax error, so the
    // third statement is never reached.
    assert_eq!(parsed.statements().len(), 1);
    assert!(!parsed.is_complete());
    let err = parsed.error().expect("a syntax error is reported");
    assert_eq!(err.kind(), googlesql::SqlErrorKind::Syntax);
}

/// Empty input yields no statements and reports the syntax error, matching the
/// single-statement `parse_statement("")` behaviour.
#[test]
fn parse_statements_on_empty_input_reports_a_syntax_error() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_statements("").unwrap();

    assert!(parsed.statements().is_empty());
    let err = parsed.error().expect("empty input is a syntax error");
    assert_eq!(err.kind(), googlesql::SqlErrorKind::Syntax);
}

/// The maximum language feature set applies to every statement in a script,
/// so gated syntax such as `QUALIFY` parses.
#[test]
fn parses_qualify_clause_in_a_script() {
    let mut module = Module::new().unwrap();
    let parsed = module
        .parse_statements("SELECT 1; SELECT x FROM t QUALIFY ROW_NUMBER() OVER (ORDER BY x) = 1")
        .unwrap();

    assert!(
        parsed.is_complete(),
        "no error expected: {:?}",
        parsed.error()
    );
    assert_eq!(parsed.statements().len(), 2);
}

/// The parser accepts DML and DDL statements, not just queries; each parses
/// into a non-empty canonical form. (Semantic acceptance is the analyzer's
/// job; here we only pin that parsing does not reject them.)
#[test]
fn parses_dml_and_ddl_statements() {
    let mut module = Module::new().unwrap();

    for sql in [
        "INSERT INTO t (a) VALUES (1)",
        "UPDATE t SET a = 1 WHERE a = 2",
        "DELETE FROM t WHERE a = 1",
        "CREATE TABLE t (a INT64)",
    ] {
        let parsed = module
            .parse_statement(sql)
            .unwrap_or_else(|e| panic!("failed to parse {sql:?}: {e:?}"));
        assert!(
            !parsed.canonical_sql().is_empty(),
            "empty canonical SQL for {sql:?}"
        );
    }
}

/// A transaction-control script parses into its individual statements.
#[test]
fn parses_transaction_control_script() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_statements("BEGIN; SELECT 1; COMMIT;").unwrap();

    assert!(
        parsed.is_complete(),
        "no error expected: {:?}",
        parsed.error()
    );
    assert_eq!(parsed.statements().len(), 3);
}

/// A bare SQL expression (not a full statement) parses into an AST and
/// round-trips through its canonical form.
#[test]
fn parses_a_bare_expression() {
    let mut module = Module::new().unwrap();
    let parsed = module.parse_expression("a + 1").unwrap();

    let canonical = parsed.canonical_sql();
    assert!(
        canonical.contains('a') && canonical.contains('+') && canonical.contains('1'),
        "canonical expression must preserve its operands and operator: {canonical:?}"
    );
    // The root is an expression node, not a statement/query node.
    assert!(
        !parsed.root().kind().is_empty(),
        "root kind: {:?}",
        parsed.root().kind()
    );
    assert!(
        parsed.root().kind().contains("Expression"),
        "expression root should be an expression node: {:?}",
        parsed.root().kind()
    );
}

/// A syntactically invalid expression returns a GoogleSql error.
#[test]
fn returns_error_for_invalid_expression() {
    let mut module = Module::new().unwrap();
    let err = module.parse_expression("1 +").unwrap_err();
    assert!(
        matches!(err, Error::GoogleSql(_)),
        "a malformed expression must produce Error::GoogleSql: {err:?}"
    );
}

/// A full statement is rejected when parsed as an expression.
#[test]
fn statement_is_not_a_valid_expression() {
    let mut module = Module::new().unwrap();
    let err = module.parse_expression("SELECT 1").unwrap_err();
    assert!(
        matches!(err, Error::GoogleSql(_)),
        "a statement is not an expression: {err:?}"
    );
}
