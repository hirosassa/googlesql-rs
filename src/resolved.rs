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
const KIND_COLUMN_REF: &str = "ResolvedColumnRef";
const KIND_LITERAL: &str = "ResolvedLiteral";
const KIND_FUNCTION_CALL: &str = "ResolvedFunctionCall";
const KIND_CAST: &str = "ResolvedCast";

/// Resolved type names (from `Type::DebugString`) of the scalar literals whose
/// values [`LiteralValue`] models.
const TYPE_INT64: &str = "INT64";
const TYPE_BOOL: &str = "BOOL";
const TYPE_STRING: &str = "STRING";
const TYPE_DOUBLE: &str = "DOUBLE";

/// `ResolvedNode` base class: `GetChildNodes` enumerates any node's children and
/// `IsExpression` reports whether a node carries a resolved type.
const SVC_RESOLVED_NODE: i32 = 1167;
const MID_GET_CHILD_NODES: i32 = 9;
const MID_IS_EXPRESSION: i32 = 17;

/// `ResolvedExpr` base class: `Type` is the expression's resolved type.
const SVC_RESOLVED_EXPR: i32 = 979;
const MID_EXPR_TYPE: i32 = 12;

/// `ResolvedColumnRef`: `Column` is the `ResolvedColumn` the reference reads.
const SVC_RESOLVED_COLUMN_REF: i32 = 849;
const MID_COLUMN_REF_COLUMN: i32 = 8;

/// `ResolvedCast`: `Expr` is the operand being cast; its resolved type is the
/// cast's source type, while the cast node's own type is the target type.
const SVC_RESOLVED_CAST: i32 = 829;
const MID_CAST_EXPR: i32 = 8;

/// `ResolvedLiteral`: `Value` is the constant the literal carries.
const SVC_RESOLVED_LITERAL: i32 = 1127;
const MID_LITERAL_VALUE: i32 = 17;

/// `Value` (zetasql::Value): scalar accessors, each returning the contents in
/// response field 1 for the matching type.
const SVC_VALUE: i32 = 1428;
const MID_VALUE_BOOL: i32 = 110;
const MID_VALUE_DOUBLE: i32 = 114;
const MID_VALUE_INT64: i32 = 128;
const MID_VALUE_STRING: i32 = 146;

/// `ResolvedFunctionCallBase`: `Function` is the catalog function the call
/// invokes, and `Function::Name` is its name (e.g. `$add`, `lower`).
const SVC_RESOLVED_FUNCTION_CALL_BASE: i32 = 1004;
const MID_FUNCTION: i32 = 19;
const SVC_FUNCTION: i32 = 636;
const MID_FUNCTION_NAME: i32 = 30;

/// `ResolvedScan` base class: `ColumnList` is the scan's referenced columns.
const SVC_RESOLVED_SCAN: i32 = 1251;
const MID_SCAN_COLUMN_LIST: i32 = 12;

/// `ResolvedTableScan`: `Table` is the catalog table the scan reads, and
/// `Table::Name` is its catalog name (the physical table, not any query alias).
const SVC_RESOLVED_TABLE_SCAN: i32 = 1295;
const MID_TABLE_SCAN_TABLE: i32 = 32;
const SVC_TABLE: i32 = 1406;
const MID_TABLE_NAME: i32 = 9;

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

/// The column a `ResolvedColumnRef` node reads.
///
/// Carried by column-reference nodes in a [`ResolvedNode`] tree; holds only
/// owned data, so it outlives the analysis that produced it.
#[derive(Debug, Clone)]
pub struct ColumnReference {
    table: String,
    name: String,
}

impl ColumnReference {
    /// The name of the table (or scan) that produces the referenced column.
    ///
    /// For a column read from a user table this is that table's name; for a
    /// column produced by an intermediate scan (a projection, aggregation, …)
    /// the analyzer supplies a synthetic name such as `$query` or `$aggregate`.
    pub fn table(&self) -> &str {
        &self.table
    }

    /// The referenced column's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The constant a `ResolvedLiteral` node carries.
///
/// Only the common scalar types are modelled. Marked `#[non_exhaustive]` because
/// GoogleSQL has more literal types than are represented here; adding a variant
/// later must not be a breaking change for callers that match on it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    /// An `INT64` constant.
    Int64(i64),
    /// A `BOOL` constant.
    Bool(bool),
    /// A `STRING` constant.
    String(String),
    /// A `DOUBLE` constant.
    Double(f64),
}

/// The source and target types of a `ResolvedCast` node.
///
/// Carried by cast nodes in a [`ResolvedNode`] tree; holds only owned data, so
/// it outlives the analysis that produced it. Groups the two ends of the
/// conversion (`from_type` → `to_type`), the way [`ColumnReference`] groups a
/// column's table and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastInfo {
    from_type: String,
    to_type: String,
}

impl CastInfo {
    /// The resolved type the cast converts from (its operand's type).
    pub fn from_type(&self) -> &str {
        &self.from_type
    }

    /// The resolved type the cast converts to (the cast's own type).
    pub fn to_type(&self) -> &str {
        &self.to_type
    }
}

/// A node in the analyzer's resolved AST.
///
/// Produced by [`Module::resolved_tree`]; a self-contained tree that holds each
/// node's kind, its resolved type (for expression nodes), the column it reads
/// (for column-reference nodes), the table it scans (for table-scan nodes), the
/// conversion it performs (for cast nodes), and its children, so it outlives the
/// analysis that built it. Use [`Module::analyze_output_columns`] or
/// [`Module::referenced_tables`] for the schema and lineage details this
/// structural view omits.
#[derive(Debug, Clone)]
pub struct ResolvedNode {
    kind: String,
    type_name: Option<String>,
    column_ref: Option<ColumnReference>,
    literal_value: Option<LiteralValue>,
    function_name: Option<String>,
    table_name: Option<String>,
    cast: Option<CastInfo>,
    children: Vec<Self>,
}

impl ResolvedNode {
    /// The node's kind (e.g. `ResolvedQueryStmt`, `ResolvedTableScan`).
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The node's resolved type name (e.g. `INT64`, `STRING`), or `None` for
    /// nodes that are not expressions (scans, statements, and other structural
    /// nodes carry no type).
    pub fn type_name(&self) -> Option<&str> {
        self.type_name.as_deref()
    }

    /// The column this node reads, or `None` if it is not a `ResolvedColumnRef`.
    pub const fn column_ref(&self) -> Option<&ColumnReference> {
        self.column_ref.as_ref()
    }

    /// The constant this node carries, or `None` if it is not a `ResolvedLiteral`
    /// (or its type is one this crate does not yet model).
    pub const fn literal_value(&self) -> Option<&LiteralValue> {
        self.literal_value.as_ref()
    }

    /// The name of the function this node invokes (e.g. `$add`, `lower`), or
    /// `None` if it is not a `ResolvedFunctionCall`.
    pub fn function_name(&self) -> Option<&str> {
        self.function_name.as_deref()
    }

    /// The catalog name of the table this node scans, or `None` if it is not a
    /// `ResolvedTableScan`. This is the physical table's name, not any query alias.
    pub fn table_name(&self) -> Option<&str> {
        self.table_name.as_deref()
    }

    /// The source and target types of this node's cast, or `None` if it is not a
    /// `ResolvedCast`.
    pub const fn cast(&self) -> Option<&CastInfo> {
        self.cast.as_ref()
    }

    /// The child nodes, in the order the analyzer reports them.
    pub fn children(&self) -> &[Self] {
        &self.children
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

    /// Builds a self-contained [`ResolvedNode`] tree from an `AnalyzerOutput`.
    ///
    /// Returns `None` for a statement that produces no resolved output, so the
    /// caller can distinguish "analyzed, but nothing to walk" from an error.
    pub(crate) fn resolved_tree_of(
        &mut self,
        analyzer_output: u64,
    ) -> Result<Option<ResolvedNode>, Error> {
        if analyzer_output == 0 {
            return Ok(None);
        }
        let statement =
            self.rpc_handle(SVC_ANALYZER_OUTPUT, MID_RESOLVED_STATEMENT, analyzer_output)?;
        if statement == 0 {
            return Ok(None);
        }
        self.build_resolved_node(statement).map(Some)
    }

    /// Recursively copies `node`'s kind, resolved type, and children into an
    /// owned tree.
    fn build_resolved_node(&mut self, node: u64) -> Result<ResolvedNode, Error> {
        let kind = self.node_kind(node)?;
        let type_name = self.node_type_name(node)?;
        let column_ref = if kind == KIND_COLUMN_REF {
            Some(self.node_column_ref(node)?)
        } else {
            None
        };
        let literal_value = if kind == KIND_LITERAL {
            self.node_literal_value(node, type_name.as_deref())?
        } else {
            None
        };
        let function_name = if kind == KIND_FUNCTION_CALL {
            Some(self.node_function_name(node)?)
        } else {
            None
        };
        let table_name = if kind == KIND_TABLE_SCAN {
            Some(self.node_table_name(node)?)
        } else {
            None
        };
        let cast = if kind == KIND_CAST {
            self.node_cast(node, type_name.as_deref())?
        } else {
            None
        };
        let mut children = Vec::new();
        for child in self.child_nodes(node)? {
            if child != 0 {
                children.push(self.build_resolved_node(child)?);
            }
        }
        Ok(ResolvedNode {
            kind,
            type_name,
            column_ref,
            literal_value,
            function_name,
            table_name,
            cast,
            children,
        })
    }

    /// Reads the source and target types of a `ResolvedCast` node.
    ///
    /// The cast is an expression, so `to_type` (its own resolved type) is already
    /// known; `expr()` yields the operand, whose type is the `from_type`. Returns
    /// `None` if the operand carries no type, which a real cast never does.
    fn node_cast(&mut self, node: u64, to_type: Option<&str>) -> Result<Option<CastInfo>, Error> {
        let Some(to_type) = to_type else {
            return Ok(None);
        };
        let operand = self.rpc_handle(SVC_RESOLVED_CAST, MID_CAST_EXPR, node)?;
        let Some(from_type) = self.node_type_name(operand)? else {
            return Ok(None);
        };
        Ok(Some(CastInfo {
            from_type,
            to_type: to_type.to_owned(),
        }))
    }

    /// Reads the catalog name of the table a `ResolvedTableScan` reads.
    ///
    /// `table()` yields the catalog `Table`, whose `Name` is the physical table
    /// name (a query alias renames the scan's output columns, not the table).
    fn node_table_name(&mut self, node: u64) -> Result<String, Error> {
        let table = self.rpc_handle(SVC_RESOLVED_TABLE_SCAN, MID_TABLE_SCAN_TABLE, node)?;
        self.rpc_string(SVC_TABLE, MID_TABLE_NAME, table)
    }

    /// Reads the catalog name of the function a `ResolvedFunctionCall` invokes.
    ///
    /// `function()` yields the `zetasql::Function`, whose `Name` is the catalog
    /// name (e.g. `$add` for `+`, `lower` for `LOWER()`).
    fn node_function_name(&mut self, node: u64) -> Result<String, Error> {
        let function = self.rpc_handle(SVC_RESOLVED_FUNCTION_CALL_BASE, MID_FUNCTION, node)?;
        self.rpc_string(SVC_FUNCTION, MID_FUNCTION_NAME, function)
    }

    /// Reads the constant a `ResolvedLiteral` node carries.
    ///
    /// The literal is an expression, so `type_name` (its resolved type) is
    /// already known; it selects which typed `Value` accessor to call. Types
    /// this crate does not model yield `None` rather than an error.
    fn node_literal_value(
        &mut self,
        node: u64,
        type_name: Option<&str>,
    ) -> Result<Option<LiteralValue>, Error> {
        let value = self.rpc_handle(SVC_RESOLVED_LITERAL, MID_LITERAL_VALUE, node)?;
        let literal = match type_name {
            Some(TYPE_INT64) => LiteralValue::Int64(self.value_int64(value, MID_VALUE_INT64)?),
            Some(TYPE_BOOL) => LiteralValue::Bool(self.value_bool(value, MID_VALUE_BOOL)?),
            Some(TYPE_STRING) => {
                LiteralValue::String(self.rpc_string(SVC_VALUE, MID_VALUE_STRING, value)?)
            }
            Some(TYPE_DOUBLE) => LiteralValue::Double(self.value_double(value, MID_VALUE_DOUBLE)?),
            _ => return Ok(None),
        };
        Ok(Some(literal))
    }

    /// Reads an int64 from a `Value` handle via the given accessor.
    fn value_int64(&mut self, value: u64, mid: i32) -> Result<i64, Error> {
        let resp = self.invoke(SVC_VALUE, mid, &pb::handle_arg(value))?;
        check_error(&resp)?;
        pb::read_int64_at_field(&resp, 1)
            .ok_or_else(|| Error::GoogleSql("int64 value not found".into()))
    }

    /// Reads a bool from a `Value` handle via the given accessor.
    fn value_bool(&mut self, value: u64, mid: i32) -> Result<bool, Error> {
        let resp = self.invoke(SVC_VALUE, mid, &pb::handle_arg(value))?;
        check_error(&resp)?;
        Ok(pb::read_bool_at_field(&resp, 1))
    }

    /// Reads a double from a `Value` handle via the given accessor.
    fn value_double(&mut self, value: u64, mid: i32) -> Result<f64, Error> {
        let resp = self.invoke(SVC_VALUE, mid, &pb::handle_arg(value))?;
        check_error(&resp)?;
        pb::read_double_at_field(&resp, 1)
            .ok_or_else(|| Error::GoogleSql("double value not found".into()))
    }

    /// Reads the `ResolvedColumn` a `ResolvedColumnRef` node points at.
    fn node_column_ref(&mut self, node: u64) -> Result<ColumnReference, Error> {
        let column = self.rpc_handle(SVC_RESOLVED_COLUMN_REF, MID_COLUMN_REF_COLUMN, node)?;
        let table = self.rpc_string(SVC_RESOLVED_COLUMN, MID_COLUMN_TABLE_NAME, column)?;
        let name = self.rpc_string(SVC_RESOLVED_COLUMN, MID_COLUMN_NAME, column)?;
        Ok(ColumnReference { table, name })
    }

    /// Returns a node's resolved type name, or `None` if it is not an expression.
    ///
    /// Only `ResolvedExpr` subclasses carry a type; asking a scan or statement
    /// for one would be meaningless, so `IsExpression` gates the lookup.
    fn node_type_name(&mut self, node: u64) -> Result<Option<String>, Error> {
        let is_expr = self.invoke(SVC_RESOLVED_NODE, MID_IS_EXPRESSION, &pb::handle_arg(node))?;
        check_error(&is_expr)?;
        if !pb::read_bool_at_field(&is_expr, 1) {
            return Ok(None);
        }

        let type_handle = self.rpc_handle(SVC_RESOLVED_EXPR, MID_EXPR_TYPE, node)?;
        if type_handle == 0 {
            return Ok(None);
        }
        self.type_debug_string(type_handle).map(Some)
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
