//! High-level API for the GoogleSQL formatter.
//!
//! `FormatSql` (svc/mid from the wasmify glue) is self-contained: it takes only
//! the SQL string and returns the pretty-printed SQL, with no options or handles.
//! Request field 1 = string(sql); response field 2 = string(formatted SQL);
//! errors arrive in field 15.

use crate::error::Error;
use crate::pb;
use crate::runtime::Module;

const SVC_FORMATTER: i32 = 0;
const MID_FORMAT_SQL: i32 = 5;

impl Module {
    /// Pretty-prints a SQL string into GoogleSQL's canonical formatted form.
    ///
    /// Returns [`Error::GoogleSql`] when the input cannot be parsed.
    pub fn format_sql(&mut self, sql: &str) -> Result<String, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, sql);
        let resp = self.invoke(SVC_FORMATTER, MID_FORMAT_SQL, &req)?;
        if let Some(message) = pb::extract_error(&resp) {
            return Err(Error::GoogleSql(message.into()));
        }
        pb::read_string_at_field(&resp, 2)
            .ok_or_else(|| Error::GoogleSql("FormatSql returned no string".into()))
    }
}
