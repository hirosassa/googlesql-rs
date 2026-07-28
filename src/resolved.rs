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

/// Node kinds (from `wasmify_get_type_name`) that the walks match on.
const KIND_QUERY_STMT: &str = "ResolvedQueryStmt";
const KIND_TABLE_SCAN: &str = "ResolvedTableScan";

/// `ResolvedNode` base class: `GetChildNodes` enumerates any node's children.
const SVC_RESOLVED_NODE: i32 = 1167;
const MID_GET_CHILD_NODES: i32 = 9;

/// `ResolvedScan` base class: `ColumnList` is the scan's referenced columns.
const SVC_RESOLVED_SCAN: i32 = 1251;
const MID_SCAN_COLUMN_LIST: i32 = 12;

const SVC_RESOLVED_QUERY_STMT: i32 = 1211;
const MID_OUTPUT_COLUMN_LIST: i32 = 12;

const SVC_RESOLVED_OUTPUT_COLUMN: i32 = 1181;
const MID_OUTPUT_COLUMN_GET_COLUMN: i32 = 8;
const MID_OUTPUT_COLUMN_NAME: i32 = 9;

const SVC_RESOLVED_COLUMN: i32 = 839;
const MID_COLUMN_NAME: i32 = 9;
const MID_COLUMN_TABLE_NAME: i32 = 11;
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

/// A table a query reads, with the columns it actually references.
///
/// Produced by [`Module::referenced_tables`]; holds only owned data.
#[derive(Debug, Clone)]
pub struct TableRef {
    name: String,
    columns: Vec<String>,
}

impl TableRef {
    /// The referenced table's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The columns read from this table, in the order first encountered.
    pub fn columns(&self) -> &[String] {
        &self.columns
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
        if analyzer_output == 0 {
            return Ok(Vec::new());
        }
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

    /// Collects the tables a resolved statement reads, with referenced columns.
    ///
    /// Returns an empty vec for statements that read no tables.
    pub(crate) fn referenced_tables_of(
        &mut self,
        analyzer_output: u64,
    ) -> Result<Vec<TableRef>, Error> {
        if analyzer_output == 0 {
            return Ok(Vec::new());
        }
        let statement =
            self.rpc_handle(SVC_ANALYZER_OUTPUT, MID_RESOLVED_STATEMENT, analyzer_output)?;
        if statement == 0 {
            return Ok(Vec::new());
        }

        let mut tables = Vec::new();
        self.collect_table_scans(statement, &mut tables)?;
        Ok(tables)
    }

    /// Records `node` if it is a table scan, then recurses into its children.
    fn collect_table_scans(&mut self, node: u64, tables: &mut Vec<TableRef>) -> Result<(), Error> {
        if self.node_kind(node)? == KIND_TABLE_SCAN {
            self.record_table_scan(node, tables)?;
        }
        for child in self.child_nodes(node)? {
            if child != 0 {
                self.collect_table_scans(child, tables)?;
            }
        }
        Ok(())
    }

    /// Adds the table and columns read by one `ResolvedTableScan` to `tables`.
    fn record_table_scan(&mut self, scan: u64, tables: &mut Vec<TableRef>) -> Result<(), Error> {
        let resp = self.invoke(
            SVC_RESOLVED_SCAN,
            MID_SCAN_COLUMN_LIST,
            &pb::handle_arg(scan),
        )?;
        check_error(&resp)?;
        for column in pb::read_handles_at_field(&resp, 1) {
            let table_name = self.rpc_string(SVC_RESOLVED_COLUMN, MID_COLUMN_TABLE_NAME, column)?;
            let column_name = self.rpc_string(SVC_RESOLVED_COLUMN, MID_COLUMN_NAME, column)?;
            add_referenced_column(tables, &table_name, column_name);
        }
        Ok(())
    }

    /// Returns the child node handles of a resolved node via `GetChildNodes`.
    fn child_nodes(&mut self, node: u64) -> Result<Vec<u64>, Error> {
        let resp = self.invoke(
            SVC_RESOLVED_NODE,
            MID_GET_CHILD_NODES,
            &pb::handle_arg(node),
        )?;
        check_error(&resp)?;
        Ok(pb::read_handles_at_field(&resp, 1))
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

/// Records `column` under `table_name`, grouping repeated tables and skipping a
/// column already recorded for that table (so lineage stays deduplicated).
fn add_referenced_column(tables: &mut Vec<TableRef>, table_name: &str, column: String) {
    if let Some(existing) = tables.iter_mut().find(|table| table.name == table_name) {
        if !existing.columns.contains(&column) {
            existing.columns.push(column);
        }
    } else {
        tables.push(TableRef {
            name: table_name.to_owned(),
            columns: vec![column],
        });
    }
}

/// Converts an error in field 15 of the response into [`Error::GoogleSql`].
fn check_error(resp: &[u8]) -> Result<(), Error> {
    pb::extract_error(resp).map_or(Ok(()), |message| Err(Error::GoogleSql(message)))
}
