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

/// An error reported by GoogleSQL itself: a syntax error, an unresolved name,
/// an unsupported feature, and so on.
///
/// The message is preserved exactly as GoogleSQL produced it (including any
/// trailing `[at line:column]` suffix); [`location`](Self::location) additionally
/// exposes that position in structured form when GoogleSQL supplied one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlError {
    message: String,
    location: Option<ErrorLocation>,
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
    #[must_use]
    pub const fn location(&self) -> Option<ErrorLocation> {
        self.location
    }
}

impl From<String> for SqlError {
    fn from(message: String) -> Self {
        let location = ErrorLocation::parse_suffix(&message);
        Self { message, location }
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
}

#[cfg(test)]
mod tests {
    use super::{Error, ErrorLocation, SqlError};

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
    fn display_reproduces_the_message_verbatim() {
        let err = SqlError::from("Table not found: t [at 1:15]");
        assert_eq!(err.to_string(), "Table not found: t [at 1:15]");
        assert_eq!(
            Error::GoogleSql(err).to_string(),
            "googlesql error: Table not found: t [at 1:15]"
        );
    }
}
