//! Typed access to the analyzer's resolved output.
//!
//! The analyzer produces a resolved AST whose leaves carry inferred types. This
//! module walks that tree once to extract the query's output schema (each column
//! name and its resolved type) into a self-contained Rust value that holds no
//! wasm handles.
//!
//! The walk borrows nodes owned by the `AnalyzerOutput` (and types owned by the
//! `TypeFactory`); it must run while both are still alive and frees nothing it
//! visits. The service/method ids were measured from the wazero reference glue.

use crate::error::Error;
use crate::pb;
use crate::runtime::Module;

const SVC_ANALYZER_OUTPUT: i32 = 558;
const MID_RESOLVED_STATEMENT: i32 = 8;

/// Node kind (from `wasmify_get_type_name`) of a resolved `SELECT` statement.
const KIND_QUERY_STMT: &str = "ResolvedQueryStmt";

const SVC_RESOLVED_QUERY_STMT: i32 = 1211;
const MID_OUTPUT_COLUMN_LIST: i32 = 12;

const SVC_RESOLVED_OUTPUT_COLUMN: i32 = 1181;
const MID_OUTPUT_COLUMN_GET_COLUMN: i32 = 8;
const MID_OUTPUT_COLUMN_NAME: i32 = 9;

const SVC_RESOLVED_COLUMN: i32 = 839;
const MID_COLUMN_TYPE: i32 = 13;

const SVC_TYPE: i32 = 1417;
const MID_TYPE_DEBUG_STRING: i32 = 14;

/// A column in a query's output schema.
///
/// Produced by [`Module::analyze_output_columns`]; holds only owned data, so it
/// outlives the analysis that created it.
#[derive(Debug, Clone)]
pub struct OutputColumn {
    name: String,
    type_name: String,
}

impl OutputColumn {
    /// The output column's name (its alias, or the source column name).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The resolved type name (e.g. `INT64`, `STRING`).
    pub fn type_name(&self) -> &str {
        &self.type_name
    }
}

impl Module {
    /// Extracts the output column schema from an `AnalyzerOutput` handle.
    ///
    /// Returns an empty vec for statements that are not queries (a query is the
    /// only kind with an output schema), so the caller need not special-case them.
    pub(crate) fn output_columns(
        &mut self,
        analyzer_output: u64,
    ) -> Result<Vec<OutputColumn>, Error> {
        let statement =
            self.rpc_handle(SVC_ANALYZER_OUTPUT, MID_RESOLVED_STATEMENT, analyzer_output)?;
        if statement == 0 || self.node_kind(statement)? != KIND_QUERY_STMT {
            return Ok(Vec::new());
        }

        let column_resp = self.invoke(
            SVC_RESOLVED_QUERY_STMT,
            MID_OUTPUT_COLUMN_LIST,
            &pb::handle_arg(statement),
        )?;
        check_error(&column_resp)?;

        pb::read_handles_at_field(&column_resp, 1)
            .into_iter()
            .map(|column| self.output_column(column))
            .collect()
    }

    /// Reads one `ResolvedOutputColumn`: its name and its column's resolved type.
    fn output_column(&mut self, output_column: u64) -> Result<OutputColumn, Error> {
        let name = self.rpc_string(
            SVC_RESOLVED_OUTPUT_COLUMN,
            MID_OUTPUT_COLUMN_NAME,
            output_column,
        )?;
        let column = self.rpc_handle(
            SVC_RESOLVED_OUTPUT_COLUMN,
            MID_OUTPUT_COLUMN_GET_COLUMN,
            output_column,
        )?;
        let type_handle = self.rpc_handle(SVC_RESOLVED_COLUMN, MID_COLUMN_TYPE, column)?;
        let type_name = self.type_debug_string(type_handle)?;
        Ok(OutputColumn { name, type_name })
    }

    /// Returns a type handle's human-readable name via `Type::DebugString(false)`.
    fn type_debug_string(&mut self, type_handle: u64) -> Result<String, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_handle);
        pb::append_bool(&mut req, 2, false); // details = false: just the type name
        let resp = self.invoke(SVC_TYPE, MID_TYPE_DEBUG_STRING, &req)?;
        check_error(&resp)?;
        pb::read_string_at_field(&resp, 1)
            .ok_or_else(|| Error::GoogleSql("type name not found".into()))
    }

    /// Common helper: passes a single handle and returns the string from field 1.
    fn rpc_string(&mut self, svc: i32, mid: i32, ptr: u64) -> Result<String, Error> {
        let resp = self.invoke(svc, mid, &pb::handle_arg(ptr))?;
        check_error(&resp)?;
        pb::read_string_at_field(&resp, 1)
            .ok_or_else(|| Error::GoogleSql("string field not found".into()))
    }
}

/// Converts an error in field 15 of the response into [`Error::GoogleSql`].
fn check_error(resp: &[u8]) -> Result<(), Error> {
    pb::extract_error(resp).map_or(Ok(()), |message| Err(Error::GoogleSql(message)))
}
