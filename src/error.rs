//! Crate-wide error type.

/// A 1-based position within analyzed SQL text.
///
/// GoogleSQL reports error positions as a `[at line:column]` suffix on its
/// messages; both coordinates count from 1, matching that convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorLocation {
    line: usize,
    column: usize,
}

impl ErrorLocation {
    /// The 1-based line number.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// The 1-based column number.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }

    /// Resolves this position to a 0-based byte offset into `sql`.
    ///
    /// GoogleSQL reports columns as 1-based Unicode code-point counts (a
    /// multi-byte character advances the column by one), so this walks the target
    /// line by code point to recover the byte offset. A column past the end of the
    /// line clamps to the line's end (GoogleSQL points one past the last character
    /// for end-of-input errors). Returns `None` when the position does not fall
    /// within `sql` (line or column out of range).
    #[must_use]
    pub fn offset(&self, sql: &str) -> Option<usize> {
        if self.line == 0 {
            return None;
        }
        let col0 = self.column.checked_sub(1)?;

        // Byte offset of the start of the target line (1-based).
        let line_start = if self.line == 1 {
            0
        } else {
            let target_newlines = self.line.checked_sub(1)?;
            let mut newlines = 0usize;
            let mut start = None;
            for (i, byte) in sql.bytes().enumerate() {
                if byte == b'\n' {
                    newlines = newlines.checked_add(1)?;
                    if newlines == target_newlines {
                        start = Some(i.checked_add(1)?);
                        break;
                    }
                }
            }
            start?
        };

        // Walk the target line by code point; a column past its end clamps to the
        // line's end (GoogleSQL points one past the last character at end of input).
        let rest = sql.get(line_start..)?;
        let line_len = rest.find('\n').unwrap_or(rest.len());
        let line = rest.get(..line_len)?;
        let within = line
            .char_indices()
            .nth(col0)
            .map_or(line.len(), |(byte, _)| byte);
        line_start.checked_add(within)
    }

    /// Parses GoogleSQL's trailing ` [at line:column]` suffix, if present.
    ///
    /// GoogleSQL appends the position as the last token of the message, e.g.
    /// `Table not found: t [at 1:15]`. A message with no such suffix (or a
    /// non-numeric one) yields `None`.
    fn parse_suffix(message: &str) -> Option<Self> {
        let inside = message.trim_end().strip_suffix(']')?.rsplit_once("[at ")?.1;
        let (line_part, column) = inside.rsplit_once(':')?;
        // A filename-qualified suffix reads `file:line:column`; keep the last
        // colon-separated component as the line number.
        let line = line_part.rsplit(':').next()?;
        Some(Self {
            line: line.parse().ok()?,
            column: column.parse().ok()?,
        })
    }
}

/// A coarse category for a [`SqlError`], derived from its message text.
///
/// The wasm ABI reports every problem as a single free-text string with no
/// separate status code, so this classification is a heuristic over the stable
/// prefixes GoogleSQL emits. The raw message is always available via
/// [`SqlError::message`] regardless of the category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlErrorKind {
    /// A parser-level syntax error. GoogleSQL's parser prefixes these with
    /// `Syntax error:` (e.g. `Syntax error: Unexpected end of statement`).
    Syntax,

    /// A feature the underlying GoogleSQL build does not support, such as
    /// `RECURSIVE` CTEs or `LIKE ANY`. Identified by the `not supported` phrase.
    Unsupported,

    /// Any other error GoogleSQL reported while resolving the statement: an
    /// unresolved name, a missing table, a type mismatch, and so on.
    Analysis,
}

impl SqlErrorKind {
    /// Classifies a GoogleSQL message into a [`SqlErrorKind`].
    ///
    /// The `Syntax error:` prefix wins over the `not supported` phrase, since a
    /// syntax error is the more specific signal when both appear.
    fn classify(message: &str) -> Self {
        if message.starts_with("Syntax error:") {
            Self::Syntax
        } else if message.contains("not supported") {
            Self::Unsupported
        } else {
            Self::Analysis
        }
    }
}

/// An error reported by GoogleSQL itself: a syntax error, an unresolved name,
/// an unsupported feature, and so on.
///
/// The message is preserved exactly as GoogleSQL produced it (including any
/// trailing `[at line:column]` suffix); [`location`](Self::location) additionally
/// exposes that position in structured form when GoogleSQL supplied one, and
/// [`kind`](Self::kind) gives a coarse category derived from the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlError {
    message: String,
    location: Option<ErrorLocation>,
    kind: SqlErrorKind,
}

impl SqlError {
    /// The error message exactly as GoogleSQL produced it, including any
    /// trailing `[at line:column]` suffix.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The source position GoogleSQL attributed the error to, or `None` when the
    /// message carried no `[at line:column]` suffix.
    ///
    /// The analyzer entry points ([`Module::analyze_statement`](crate::Module::analyze_statement)
    /// and friends) attach this suffix to every error, including syntax errors
    /// they surface while parsing. Errors from
    /// [`Module::parse_statement`](crate::Module::parse_statement), however, arrive
    /// without a position: GoogleSQL emits parser errors with the location in a
    /// structured payload that the wasm boundary drops, and the parser's error
    /// message mode is not settable through the exposed ABI. For a located syntax
    /// error, analyze the statement instead of parsing it.
    #[must_use]
    pub const fn location(&self) -> Option<ErrorLocation> {
        self.location
    }

    /// The coarse category of this error, derived from its message text.
    ///
    /// See [`SqlErrorKind`] for the caveats on this heuristic classification.
    #[must_use]
    pub const fn kind(&self) -> SqlErrorKind {
        self.kind
    }

    /// Renders the offending source line with a caret (`^`) under the error
    /// position, mirroring GoogleSQL's multi-line-with-caret error format.
    ///
    /// For example, an error at `1:8` of `SELECT a FROM b` renders as:
    ///
    /// ```text
    /// SELECT a FROM b
    ///        ^
    /// ```
    ///
    /// The caret is padded with `column - 1` spaces, matching GoogleSQL's own
    /// convention (one space per column, so a run of wide characters shifts the
    /// caret by their code-point count, not their display width). Returns `None`
    /// when this error carries no location or the location's line is not in `sql`.
    #[must_use]
    pub fn caret_snippet(&self, sql: &str) -> Option<String> {
        let location = self.location?;
        if location.line == 0 {
            return None;
        }
        let pad = location.column.checked_sub(1)?;
        let line_index = location.line.checked_sub(1)?;
        let line = sql.split('\n').nth(line_index)?;
        Some(format!("{line}\n{}^", " ".repeat(pad)))
    }
}

impl From<String> for SqlError {
    fn from(message: String) -> Self {
        let location = ErrorLocation::parse_suffix(&message);
        let kind = SqlErrorKind::classify(&message);
        Self {
            message,
            location,
            kind,
        }
    }
}

impl From<&str> for SqlError {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl std::fmt::Display for SqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Errors that can occur when using the GoogleSQL bindings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Failed to load or instantiate the wasm module.
    #[error("failed to instantiate googlesql wasm: {0}")]
    Instantiate(String),

    /// A wasm runtime error (e.g. failed export call).
    #[error("wasm runtime error: {0}")]
    Wasm(String),

    /// Failed to read from or write to wasm linear memory.
    #[error("wasm memory access error: {0}")]
    Memory(String),

    /// An error returned by GoogleSQL itself (e.g. a syntax error).
    #[error("googlesql error: {0}")]
    GoogleSql(SqlError),

    /// The wasm module returned a response the bindings could not interpret: a
    /// null handle where one was required, a missing response field, or an enum
    /// value the ABI does not recognize.
    ///
    /// Unlike [`GoogleSql`](Self::GoogleSql), this signals a contract mismatch
    /// between these bindings and the wasm module — not a problem with the
    /// user's SQL — so it is kept out of [`SqlError`] classification entirely.
    #[error("unexpected googlesql wasm response: {0}")]
    Protocol(String),
}

/// Converts a GoogleSQL error carried in field 15 of a wasm response into
/// [`Error::GoogleSql`].
///
/// The wasm ABI reports every GoogleSQL-level problem as a single free-text
/// string in field 15 of the response; its absence means the call succeeded.
/// This is the single place that mapping lives, shared by every module that
/// invokes the wasm surface.
pub fn check_error(resp: &[u8]) -> Result<(), Error> {
    crate::pb::extract_error(resp).map_or(Ok(()), |message| Err(Error::GoogleSql(message.into())))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        reason = "test code"
    )]

    use super::{Error, ErrorLocation, SqlError, SqlErrorKind};

    #[test]
    fn offset_resolves_ascii_single_line() {
        let loc = ErrorLocation { line: 1, column: 8 };
        // Column 8 is the `a` in `SELECT a`.
        assert_eq!(loc.offset("SELECT a FROM b"), Some(7));
    }

    #[test]
    fn offset_counts_columns_as_code_points_not_bytes() {
        // GoogleSQL reports `missing_col` at column 20 even though the multi-byte
        // string literal precedes it; the byte offset must land on `m`.
        let sql = "SELECT '日本語' AS x, missing_col";
        let loc = ErrorLocation {
            line: 1,
            column: 20,
        };
        let offset = loc.offset(sql).expect("in range");
        assert!(sql[offset..].starts_with("missing_col"), "offset {offset}");
    }

    #[test]
    fn offset_handles_a_later_line() {
        let sql = "SELECT\n  missing_col\nFROM users";
        let loc = ErrorLocation { line: 2, column: 3 };
        let offset = loc.offset(sql).expect("in range");
        assert!(sql[offset..].starts_with("missing_col"), "offset {offset}");
    }

    #[test]
    fn offset_at_end_of_input_clamps_to_the_line_end() {
        // `SELECT a FROM` is 13 characters; GoogleSQL points one past the end.
        let sql = "SELECT a FROM";
        let loc = ErrorLocation {
            line: 1,
            column: 14,
        };
        assert_eq!(loc.offset(sql), Some(sql.len()));
    }

    #[test]
    fn offset_out_of_range_line_yields_none() {
        let loc = ErrorLocation { line: 5, column: 1 };
        assert_eq!(loc.offset("SELECT 1"), None);
    }

    #[test]
    fn offset_rejects_zero_column() {
        let loc = ErrorLocation { line: 1, column: 0 };
        assert_eq!(loc.offset("SELECT 1"), None);
    }

    #[test]
    fn caret_snippet_points_under_the_error_column() {
        let err = SqlError::from("Syntax error: boom [at 1:8]");
        assert_eq!(
            err.caret_snippet("SELECT a FROM b").as_deref(),
            Some("SELECT a FROM b\n       ^")
        );
    }

    #[test]
    fn caret_snippet_selects_the_offending_line() {
        let err = SqlError::from("Unrecognized name: missing_col [at 2:3]");
        assert_eq!(
            err.caret_snippet("SELECT\n  missing_col\nFROM users")
                .as_deref(),
            Some("  missing_col\n  ^")
        );
    }

    #[test]
    fn caret_snippet_without_location_yields_none() {
        let err = SqlError::from("ParseStatement returned null");
        assert_eq!(err.caret_snippet("SELECT 1"), None);
    }

    #[test]
    fn parses_line_and_column_from_suffix() {
        let err = SqlError::from("Table not found: missing_table [at 1:15]");
        assert_eq!(
            err.location(),
            Some(ErrorLocation {
                line: 1,
                column: 15
            })
        );
        // The message is preserved verbatim, suffix included.
        assert_eq!(err.message(), "Table not found: missing_table [at 1:15]");
    }

    #[test]
    fn parses_multi_line_position() {
        let err = SqlError::from("Unrecognized name: bad_col [at 2:3]");
        assert_eq!(err.location(), Some(ErrorLocation { line: 2, column: 3 }));
    }

    #[test]
    fn parses_column_one() {
        let err = SqlError::from("Syntax error: Unexpected identifier \"X\" [at 1:1]");
        assert_eq!(err.location(), Some(ErrorLocation { line: 1, column: 1 }));
    }

    #[test]
    fn keeps_last_two_components_of_filename_qualified_suffix() {
        let err = SqlError::from("boom [at query.sql:5:6]");
        assert_eq!(err.location(), Some(ErrorLocation { line: 5, column: 6 }));
    }

    #[test]
    fn tolerates_trailing_whitespace() {
        let err = SqlError::from("boom [at 3:4]   ");
        assert_eq!(err.location(), Some(ErrorLocation { line: 3, column: 4 }));
    }

    #[test]
    fn no_suffix_yields_no_location_but_keeps_message() {
        let err = SqlError::from("ParseStatement returned null");
        assert_eq!(err.location(), None);
        assert_eq!(err.message(), "ParseStatement returned null");
    }

    #[test]
    fn non_numeric_suffix_yields_no_location() {
        let err = SqlError::from("weird [at x:y]");
        assert_eq!(err.location(), None);
    }

    #[test]
    fn rejects_negative_coordinates() {
        // Positions are 1-based unsigned; a negative component cannot parse as
        // usize, so the whole suffix is treated as absent.
        assert_eq!(SqlError::from("boom [at -1:5]").location(), None);
        assert_eq!(SqlError::from("boom [at 1:-5]").location(), None);
    }

    #[test]
    fn rejects_overflowing_coordinates() {
        // A number too large for usize must not wrap or panic; it yields None.
        let huge = format!("boom [at {}0:1]", usize::MAX);
        assert_eq!(SqlError::from(huge).location(), None);
    }

    #[test]
    fn rejects_suffix_with_internal_whitespace() {
        // GoogleSQL writes `[at L:C]` with no spaces around the colon; a spaced
        // variant fails to parse rather than being silently coerced.
        assert_eq!(SqlError::from("boom [at 1 : 5]").location(), None);
    }

    #[test]
    fn rejects_suffix_missing_closing_bracket() {
        assert_eq!(SqlError::from("boom [at 1:5").location(), None);
    }

    #[test]
    fn keeps_last_two_of_three_or_more_colon_components() {
        // Beyond `file:line:column`, only the final line:column pair is used.
        let err = SqlError::from("boom [at a:b:7:8]");
        assert_eq!(err.location(), Some(ErrorLocation { line: 7, column: 8 }));
    }

    #[test]
    fn classification_is_case_sensitive_and_prefix_anchored() {
        // The `Syntax error:` signal must be an exact, leading match: a
        // differently-cased or non-leading occurrence is not a syntax error.
        assert_eq!(
            SqlError::from("syntax error: x").kind(),
            SqlErrorKind::Analysis
        );
        assert_eq!(
            SqlError::from("SYNTAX ERROR: x").kind(),
            SqlErrorKind::Analysis
        );
        assert_eq!(
            SqlError::from(" Syntax error: leading space").kind(),
            SqlErrorKind::Analysis
        );
    }

    #[test]
    fn unsupported_phrase_matches_anywhere_in_the_message() {
        // Unlike the syntax prefix, `not supported` is detected wherever it
        // appears, since GoogleSQL phrases the feature name first.
        assert_eq!(
            SqlError::from("Feature FOO not supported here [at 1:1]").kind(),
            SqlErrorKind::Unsupported
        );
    }

    #[test]
    fn protocol_error_displays_with_its_own_framing() {
        // An internal ABI failure (null handle, missing field, unrecognized
        // enum) is a `Protocol` error, framed distinctly from a GoogleSQL query
        // error so callers can tell "the SQL was bad" from "the binding and the
        // wasm module disagree".
        let err = Error::Protocol("ParseStatement returned null".to_owned());
        assert_eq!(
            err.to_string(),
            "unexpected googlesql wasm response: ParseStatement returned null"
        );
        assert!(!matches!(err, Error::GoogleSql(_)));
    }

    #[test]
    fn display_reproduces_the_message_verbatim() {
        let err = SqlError::from("Table not found: t [at 1:15]");
        assert_eq!(err.to_string(), "Table not found: t [at 1:15]");
        assert_eq!(
            Error::GoogleSql(err).to_string(),
            "googlesql error: Table not found: t [at 1:15]"
        );
    }

    #[test]
    fn classifies_parser_syntax_errors() {
        for message in [
            "Syntax error: SELECT list must not be empty",
            "Syntax error: Unexpected end of statement",
            "Syntax error: Unclosed string literal",
        ] {
            assert_eq!(SqlError::from(message).kind(), SqlErrorKind::Syntax);
        }
    }

    #[test]
    fn classifies_unsupported_feature_errors() {
        for message in [
            "RECURSIVE is not supported in the WITH clause [at 1:1]",
            "LIKE ANY is not supported [at 1:31]",
            "Analytic functions not supported [at 1:8]",
        ] {
            assert_eq!(SqlError::from(message).kind(), SqlErrorKind::Unsupported);
        }
    }

    #[test]
    fn classifies_remaining_errors_as_analysis() {
        for message in [
            "Table not found: missing_table [at 1:15]",
            "Unrecognized name: missing_col [at 1:8]",
            "No matching signature for operator = for argument types: INT64, STRING [at 1:28]",
        ] {
            assert_eq!(SqlError::from(message).kind(), SqlErrorKind::Analysis);
        }
    }

    #[test]
    fn a_message_matching_no_pattern_falls_through_to_analysis() {
        // The internal RPC-failure messages that get wrapped in `GoogleSql`
        // (e.g. a null handle) match neither pattern and land here.
        assert_eq!(SqlError::from("").kind(), SqlErrorKind::Analysis);
        assert_eq!(
            SqlError::from("ParseStatement returned null").kind(),
            SqlErrorKind::Analysis
        );
    }

    #[test]
    fn syntax_prefix_takes_precedence_over_unsupported_phrase() {
        // A parser message that happens to mention the phrase is still a syntax
        // error: the `Syntax error:` prefix is the stronger signal.
        let err = SqlError::from("Syntax error: feature is not supported here");
        assert_eq!(err.kind(), SqlErrorKind::Syntax);
    }
}
