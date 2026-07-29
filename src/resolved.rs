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

use std::ops::Range;

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
const KIND_AGGREGATE_FUNCTION_CALL: &str = "ResolvedAggregateFunctionCall";
const KIND_CAST: &str = "ResolvedCast";
const KIND_PARAMETER: &str = "ResolvedParameter";
const KIND_JOIN_SCAN: &str = "ResolvedJoinScan";
const KIND_ORDER_BY_ITEM: &str = "ResolvedOrderByItem";
const KIND_SET_OPERATION_SCAN: &str = "ResolvedSetOperationScan";
const KIND_AGGREGATE_SCAN: &str = "ResolvedAggregateScan";
const KIND_LIMIT_OFFSET_SCAN: &str = "ResolvedLimitOffsetScan";

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
/// `GetParseLocationRangeOrNULL` returns the node's `ParseLocationRange`, or a
/// null handle when the analyzer recorded no location for it.
const MID_PARSE_LOCATION_RANGE: i32 = 15;

/// `ParseLocationRange`: `Start`/`End` are the range's `ParseLocationPoint`s.
const SVC_PARSE_LOCATION_RANGE: i32 = 693;
const MID_RANGE_START: i32 = 13;
const MID_RANGE_END: i32 = 8;

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

/// `ResolvedParameter`: `Name` is the query parameter's name (e.g. `p` for `@p`).
const SVC_RESOLVED_PARAMETER: i32 = 1185;
const MID_PARAMETER_NAME: i32 = 9;

/// `ResolvedJoinScan`: `JoinType` is the join's kind, as a
/// `ResolvedJoinScanEnums::JoinType` (1=INNER, 2=LEFT, 3=RIGHT, 4=FULL).
const SVC_RESOLVED_JOIN_SCAN: i32 = 1123;
const MID_JOIN_TYPE: i32 = 12;
const JOIN_TYPE_INNER: i32 = 1;
const JOIN_TYPE_LEFT: i32 = 2;
const JOIN_TYPE_RIGHT: i32 = 3;
const JOIN_TYPE_FULL: i32 = 4;

/// `ResolvedOrderByItem`: `IsDescending` is whether the item sorts `DESC`.
const SVC_RESOLVED_ORDER_BY_ITEM: i32 = 1177;
const MID_IS_DESCENDING: i32 = 11;

/// `ResolvedSetOperationScan`: `OpType` is the set operator, as a
/// `ResolvedSetOperationScanEnums::SetOperationType` (1=UNION ALL,
/// 2=UNION DISTINCT, 3=INTERSECT ALL, 4=INTERSECT DISTINCT, 5=EXCEPT ALL,
/// 6=EXCEPT DISTINCT).
const SVC_RESOLVED_SET_OPERATION_SCAN: i32 = 1261;
const MID_OP_TYPE: i32 = 16;
const OP_TYPE_UNION_ALL: i32 = 1;
const OP_TYPE_UNION_DISTINCT: i32 = 2;
const OP_TYPE_INTERSECT_ALL: i32 = 3;
const OP_TYPE_INTERSECT_DISTINCT: i32 = 4;
const OP_TYPE_EXCEPT_ALL: i32 = 5;
const OP_TYPE_EXCEPT_DISTINCT: i32 = 6;

/// `ResolvedAggregateScanBase`: `GroupByList` holds the `ResolvedComputedColumn`s
/// that define the query's grouping keys.
const SVC_RESOLVED_AGGREGATE_SCAN_BASE: i32 = 730;
const MID_GROUP_BY_LIST: i32 = 21;

/// `ResolvedComputedColumn`: `Column` is the `ResolvedColumn` it defines.
const SVC_RESOLVED_COMPUTED_COLUMN: i32 = 853;
const MID_COMPUTED_COLUMN_COLUMN: i32 = 8;

/// `ResolvedLimitOffsetScan`: `Limit`/`Offset` are the row-count expressions
/// (each a literal or parameter, or a null handle when the clause is absent).
const SVC_RESOLVED_LIMIT_OFFSET_SCAN: i32 = 1125;
const MID_LIMIT: i32 = 9;
const MID_OFFSET: i32 = 12;

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
const MID_ARGUMENT_LIST_SIZE: i32 = 14;
const SVC_FUNCTION: i32 = 636;
const MID_FUNCTION_NAME: i32 = 30;

/// `ResolvedNonScalarFunctionCallBase`: `Distinct` is whether the aggregate or
/// analytic call applies DISTINCT (e.g. `COUNT(DISTINCT x)`). Scalar
/// `ResolvedFunctionCall`s do not inherit this base.
const SVC_RESOLVED_NON_SCALAR_FUNCTION_CALL_BASE: i32 = 1169;
const MID_DISTINCT: i32 = 8;

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

/// The kind of a `ResolvedJoinScan`.
///
/// Marked `#[non_exhaustive]` because GoogleSQL may report join kinds beyond
/// these (e.g. semi/anti joins from array or `IN` subquery rewrites); adding a
/// variant later must not be a breaking change for callers that match on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JoinType {
    /// `INNER JOIN` (also a comma/cross join with an `ON`/`USING` condition).
    Inner,
    /// `LEFT [OUTER] JOIN`.
    Left,
    /// `RIGHT [OUTER] JOIN`.
    Right,
    /// `FULL [OUTER] JOIN`.
    Full,
}

/// The kind of a `ResolvedSetOperationScan`.
///
/// Each SQL set operator has an `ALL` form (keeps duplicates) and a `DISTINCT`
/// form (removes them); a bare `UNION`/`INTERSECT`/`EXCEPT` is the `DISTINCT`
/// form. Marked `#[non_exhaustive]` for parity with [`JoinType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SetOperation {
    /// `UNION ALL`.
    UnionAll,
    /// `UNION [DISTINCT]`.
    UnionDistinct,
    /// `INTERSECT ALL`.
    IntersectAll,
    /// `INTERSECT [DISTINCT]`.
    IntersectDistinct,
    /// `EXCEPT ALL`.
    ExceptAll,
    /// `EXCEPT [DISTINCT]`.
    ExceptDistinct,
}

/// The row counts of a `ResolvedLimitOffsetScan`.
///
/// Carried by limit/offset scan nodes in a [`ResolvedNode`] tree; holds only
/// owned data. Each field is the constant value of its clause when that clause
/// is an `INT64` literal, and `None` when the clause is absent (a bare `LIMIT`
/// has no offset) or its value is not an integer literal (e.g. a query
/// parameter such as `LIMIT @n`). Groups the two related counts the way
/// [`CastInfo`] groups a cast's two ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitOffset {
    limit: Option<i64>,
    offset: Option<i64>,
}

impl LimitOffset {
    /// The `LIMIT` row count, or `None` if it is not an `INT64` literal.
    pub const fn limit(&self) -> Option<i64> {
        self.limit
    }

    /// The `OFFSET` row count, or `None` if there is no `OFFSET` clause or its
    /// value is not an `INT64` literal.
    pub const fn offset(&self) -> Option<i64> {
        self.offset
    }
}

/// A node in the analyzer's resolved AST.
///
/// Produced by [`Module::resolved_tree`]; a self-contained tree that holds each
/// node's kind, its resolved type (for expression nodes), the column it reads
/// (for column-reference nodes), the table it scans (for table-scan nodes), the
/// conversion it performs (for cast nodes), the parameter it references (for
/// parameter nodes), and its children, so it outlives the analysis that built
/// it. Use [`Module::analyze_output_columns`] or [`Module::referenced_tables`]
/// for the schema and lineage details this structural view omits.
#[derive(Debug, Clone)]
pub struct ResolvedNode {
    kind: String,
    type_name: Option<String>,
    column_ref: Option<ColumnReference>,
    literal_value: Option<LiteralValue>,
    function_name: Option<String>,
    table_name: Option<String>,
    cast: Option<CastInfo>,
    parameter_name: Option<String>,
    argument_count: Option<usize>,
    scan_columns: Option<Vec<String>>,
    distinct: Option<bool>,
    join_type: Option<JoinType>,
    is_descending: Option<bool>,
    set_operation: Option<SetOperation>,
    group_by_columns: Option<Vec<String>>,
    limit_offset: Option<LimitOffset>,
    parse_location: Option<Range<usize>>,
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

    /// The name of the function this node invokes (e.g. `$add`, `lower`,
    /// `count`), or `None` if it is not a function call. Covers both scalar
    /// (`ResolvedFunctionCall`) and aggregate (`ResolvedAggregateFunctionCall`)
    /// calls; use [`is_aggregate`](Self::is_aggregate) to tell them apart.
    pub fn function_name(&self) -> Option<&str> {
        self.function_name.as_deref()
    }

    /// Whether this node is an aggregate function call (e.g. `COUNT`, `SUM`), as
    /// opposed to a scalar call or a non-call node.
    pub fn is_aggregate(&self) -> bool {
        self.kind == KIND_AGGREGATE_FUNCTION_CALL
    }

    /// The catalog name of the table this node scans, or `None` if it is not a
    /// `ResolvedTableScan`. This is the physical table's name, not any query alias.
    pub fn table_name(&self) -> Option<&str> {
        self.table_name.as_deref()
    }

    /// The names of the columns this node's table scan produces, or `None` if it
    /// is not a `ResolvedTableScan`. Because [`Module::resolved_tree`] does not
    /// prune columns, this lists every column of the scanned table, not only the
    /// ones the query references (use [`Module::referenced_tables`] for the
    /// referenced-only, query-wide view).
    pub fn scan_columns(&self) -> Option<&[String]> {
        self.scan_columns.as_deref()
    }

    /// The source and target types of this node's cast, or `None` if it is not a
    /// `ResolvedCast`.
    pub const fn cast(&self) -> Option<&CastInfo> {
        self.cast.as_ref()
    }

    /// The name of the query parameter this node references (e.g. `p` for `@p`),
    /// or `None` if it is not a `ResolvedParameter`.
    pub fn parameter_name(&self) -> Option<&str> {
        self.parameter_name.as_deref()
    }

    /// The number of value arguments this node's function call takes, or `None`
    /// if it is not a function call. Covers both scalar
    /// (`ResolvedFunctionCall`) and aggregate (`ResolvedAggregateFunctionCall`)
    /// calls. For a scalar call this equals `children().len()`, but an aggregate
    /// call may carry extra modifier children (e.g. `ORDER BY`), so this counts
    /// only the value arguments.
    pub const fn argument_count(&self) -> Option<usize> {
        self.argument_count
    }

    /// Whether this node's aggregate function call applies DISTINCT
    /// (e.g. `COUNT(DISTINCT x)`), or `None` if it is not an aggregate function
    /// call. Scalar function calls never carry DISTINCT, so they report `None`,
    /// not `Some(false)`.
    pub const fn distinct(&self) -> Option<bool> {
        self.distinct
    }

    /// The kind of this node's join (INNER/LEFT/RIGHT/FULL), or `None` if it is
    /// not a `ResolvedJoinScan`.
    pub const fn join_type(&self) -> Option<JoinType> {
        self.join_type
    }

    /// Whether this ORDER BY item sorts descending (`DESC`), or `None` if it is
    /// not a `ResolvedOrderByItem`. Ascending is the default, so an item written
    /// without `ASC`/`DESC` reports `Some(false)`.
    pub const fn is_descending(&self) -> Option<bool> {
        self.is_descending
    }

    /// The set operator this node applies (`UNION`/`INTERSECT`/`EXCEPT`, in its
    /// `ALL` or `DISTINCT` form), or `None` if it is not a
    /// `ResolvedSetOperationScan`.
    pub const fn set_operation(&self) -> Option<SetOperation> {
        self.set_operation
    }

    /// The names of this node's `GROUP BY` columns, or `None` if it is not a
    /// `ResolvedAggregateScan`. Grouping keys keep the order they are written;
    /// an aggregate scan with no keys (a bare aggregate such as
    /// `SELECT COUNT(*)`) reports `Some([])`.
    pub fn group_by_columns(&self) -> Option<&[String]> {
        self.group_by_columns.as_deref()
    }

    /// The `LIMIT`/`OFFSET` row counts of this node, or `None` if it is not a
    /// `ResolvedLimitOffsetScan`.
    pub const fn limit_offset(&self) -> Option<&LimitOffset> {
        self.limit_offset.as_ref()
    }

    /// The byte range this node spans within the analyzed SQL, or `None` if the
    /// analyzer recorded no location for it. [`Module::resolved_tree`] requests
    /// full-node-scope location recording, but synthetic nodes with no source
    /// text (e.g. a wrapping projection) still report `None`. The range indexes
    /// the same SQL string passed to `resolved_tree`.
    pub fn parse_location(&self) -> Option<Range<usize>> {
        self.parse_location.clone()
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
        let is_function_call = kind == KIND_FUNCTION_CALL || kind == KIND_AGGREGATE_FUNCTION_CALL;
        let function_name = if is_function_call {
            Some(self.node_function_name(node)?)
        } else {
            None
        };
        let argument_count = if is_function_call {
            Some(self.node_argument_count(node)?)
        } else {
            None
        };
        let table_name = if kind == KIND_TABLE_SCAN {
            Some(self.node_table_name(node)?)
        } else {
            None
        };
        let scan_columns = if kind == KIND_TABLE_SCAN {
            Some(self.node_scan_columns(node)?)
        } else {
            None
        };
        let distinct = if kind == KIND_AGGREGATE_FUNCTION_CALL {
            Some(self.node_distinct(node)?)
        } else {
            None
        };
        let join_type = if kind == KIND_JOIN_SCAN {
            Some(self.node_join_type(node)?)
        } else {
            None
        };
        let is_descending = if kind == KIND_ORDER_BY_ITEM {
            Some(self.node_is_descending(node)?)
        } else {
            None
        };
        let set_operation = if kind == KIND_SET_OPERATION_SCAN {
            Some(self.node_set_operation(node)?)
        } else {
            None
        };
        let group_by_columns = if kind == KIND_AGGREGATE_SCAN {
            Some(self.node_group_by_columns(node)?)
        } else {
            None
        };
        let limit_offset = if kind == KIND_LIMIT_OFFSET_SCAN {
            Some(self.node_limit_offset(node)?)
        } else {
            None
        };
        // A location can attach to any resolved node, so this is not gated on kind.
        let parse_location = self.node_parse_location(node)?;
        let cast = if kind == KIND_CAST {
            self.node_cast(node, type_name.as_deref())?
        } else {
            None
        };
        let parameter_name = if kind == KIND_PARAMETER {
            Some(self.rpc_string(SVC_RESOLVED_PARAMETER, MID_PARAMETER_NAME, node)?)
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
            parameter_name,
            argument_count,
            scan_columns,
            distinct,
            join_type,
            is_descending,
            set_operation,
            group_by_columns,
            limit_offset,
            parse_location,
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

    /// Reads the names of the columns a `ResolvedTableScan` produces.
    ///
    /// `column_list()` yields the scan's `ResolvedColumn`s (unpruned, so every
    /// table column); each column's `Name` is its name within the table.
    fn node_scan_columns(&mut self, node: u64) -> Result<Vec<String>, Error> {
        let resp = self.invoke(
            SVC_RESOLVED_SCAN,
            MID_SCAN_COLUMN_LIST,
            &pb::handle_arg(node),
        )?;
        check_error(&resp)?;
        pb::read_handles_at_field(&resp, 1)
            .into_iter()
            .map(|column| self.rpc_string(SVC_RESOLVED_COLUMN, MID_COLUMN_NAME, column))
            .collect()
    }

    /// Reads whether an aggregate function call applies DISTINCT.
    ///
    /// `distinct()` is a bool on `ResolvedNonScalarFunctionCallBase`. proto3
    /// omits a false value, so a missing field means the call is not DISTINCT.
    fn node_distinct(&mut self, node: u64) -> Result<bool, Error> {
        let resp = self.invoke(
            SVC_RESOLVED_NON_SCALAR_FUNCTION_CALL_BASE,
            MID_DISTINCT,
            &pb::handle_arg(node),
        )?;
        check_error(&resp)?;
        Ok(pb::read_bool_at_field(&resp, 1))
    }

    /// Reads the source byte range a resolved node spans, if one was recorded.
    ///
    /// `GetParseLocationRangeOrNULL` yields a `ParseLocationRange` (or a null
    /// handle for nodes with no recorded location); its `Start`/`End` are
    /// `ParseLocationPoint`s that [`Module::byte_range_from_points`] turns into a
    /// validated byte range, shared with the parser AST's location logic.
    fn node_parse_location(&mut self, node: u64) -> Result<Option<Range<usize>>, Error> {
        let range = self.rpc_handle(SVC_RESOLVED_NODE, MID_PARSE_LOCATION_RANGE, node)?;
        if range == 0 {
            return Ok(None);
        }
        let start_point = self.rpc_handle(SVC_PARSE_LOCATION_RANGE, MID_RANGE_START, range)?;
        let end_point = self.rpc_handle(SVC_PARSE_LOCATION_RANGE, MID_RANGE_END, range)?;
        self.byte_range_from_points(start_point, end_point)
    }

    /// Reads the kind of a `ResolvedJoinScan`.
    ///
    /// `join_type()` is a `ResolvedJoinScanEnums::JoinType`. The value always
    /// names a concrete join kind (the enum reserves no zero default), so an
    /// unrecognized or missing value is surfaced as an error rather than being
    /// silently mapped to a default.
    fn node_join_type(&mut self, node: u64) -> Result<JoinType, Error> {
        let resp = self.invoke(SVC_RESOLVED_JOIN_SCAN, MID_JOIN_TYPE, &pb::handle_arg(node))?;
        check_error(&resp)?;
        match pb::read_int32_at_field(&resp, 1) {
            Some(JOIN_TYPE_INNER) => Ok(JoinType::Inner),
            Some(JOIN_TYPE_LEFT) => Ok(JoinType::Left),
            Some(JOIN_TYPE_RIGHT) => Ok(JoinType::Right),
            Some(JOIN_TYPE_FULL) => Ok(JoinType::Full),
            other => Err(Error::GoogleSql(format!("unknown join type: {other:?}"))),
        }
    }

    /// Reads whether a `ResolvedOrderByItem` sorts descending.
    ///
    /// `is_descending()` is a bool; proto3 omits a false value, so a missing
    /// field means the item sorts ascending (the default).
    fn node_is_descending(&mut self, node: u64) -> Result<bool, Error> {
        let resp = self.invoke(
            SVC_RESOLVED_ORDER_BY_ITEM,
            MID_IS_DESCENDING,
            &pb::handle_arg(node),
        )?;
        check_error(&resp)?;
        Ok(pb::read_bool_at_field(&resp, 1))
    }

    /// Reads the set operator of a `ResolvedSetOperationScan`.
    ///
    /// `op_type()` is a `ResolvedSetOperationScanEnums::SetOperationType`. As
    /// with a join kind, the value always names a concrete operator (no zero
    /// default), so an unrecognized or missing value is surfaced as an error.
    fn node_set_operation(&mut self, node: u64) -> Result<SetOperation, Error> {
        let resp = self.invoke(
            SVC_RESOLVED_SET_OPERATION_SCAN,
            MID_OP_TYPE,
            &pb::handle_arg(node),
        )?;
        check_error(&resp)?;
        match pb::read_int32_at_field(&resp, 1) {
            Some(OP_TYPE_UNION_ALL) => Ok(SetOperation::UnionAll),
            Some(OP_TYPE_UNION_DISTINCT) => Ok(SetOperation::UnionDistinct),
            Some(OP_TYPE_INTERSECT_ALL) => Ok(SetOperation::IntersectAll),
            Some(OP_TYPE_INTERSECT_DISTINCT) => Ok(SetOperation::IntersectDistinct),
            Some(OP_TYPE_EXCEPT_ALL) => Ok(SetOperation::ExceptAll),
            Some(OP_TYPE_EXCEPT_DISTINCT) => Ok(SetOperation::ExceptDistinct),
            other => Err(Error::GoogleSql(format!(
                "unknown set operation: {other:?}"
            ))),
        }
    }

    /// Reads the names of a `ResolvedAggregateScan`'s `GROUP BY` columns.
    ///
    /// `group_by_list()` yields the `ResolvedComputedColumn`s that define the
    /// grouping keys; each one's `column()` is the `ResolvedColumn` naming it.
    fn node_group_by_columns(&mut self, node: u64) -> Result<Vec<String>, Error> {
        let resp = self.invoke(
            SVC_RESOLVED_AGGREGATE_SCAN_BASE,
            MID_GROUP_BY_LIST,
            &pb::handle_arg(node),
        )?;
        check_error(&resp)?;
        pb::read_handles_at_field(&resp, 1)
            .into_iter()
            .map(|computed| {
                let column = self.rpc_handle(
                    SVC_RESOLVED_COMPUTED_COLUMN,
                    MID_COMPUTED_COLUMN_COLUMN,
                    computed,
                )?;
                self.rpc_string(SVC_RESOLVED_COLUMN, MID_COLUMN_NAME, column)
            })
            .collect()
    }

    /// Reads the `LIMIT`/`OFFSET` row counts of a `ResolvedLimitOffsetScan`.
    ///
    /// `limit()`/`offset()` are expressions (a null handle for an absent
    /// clause); each contributes a value only when it is an `INT64` literal.
    fn node_limit_offset(&mut self, node: u64) -> Result<LimitOffset, Error> {
        let limit_expr = self.rpc_handle(SVC_RESOLVED_LIMIT_OFFSET_SCAN, MID_LIMIT, node)?;
        let offset_expr = self.rpc_handle(SVC_RESOLVED_LIMIT_OFFSET_SCAN, MID_OFFSET, node)?;
        Ok(LimitOffset {
            limit: self.literal_int64_value(limit_expr)?,
            offset: self.literal_int64_value(offset_expr)?,
        })
    }

    /// Reads an expression's value as an `i64` when it is an `INT64` literal.
    ///
    /// Returns `None` for a null handle (an absent clause) or any expression
    /// that is not an integer literal (e.g. a query parameter), reusing the
    /// literal machinery so the type dispatch stays in one place.
    fn literal_int64_value(&mut self, expr: u64) -> Result<Option<i64>, Error> {
        if expr == 0 || self.node_kind(expr)? != KIND_LITERAL {
            return Ok(None);
        }
        let type_name = self.node_type_name(expr)?;
        match self.node_literal_value(expr, type_name.as_deref())? {
            Some(LiteralValue::Int64(value)) => Ok(Some(value)),
            _ => Ok(None),
        }
    }

    /// Reads the catalog name of the function a `ResolvedFunctionCall` invokes.
    ///
    /// `function()` yields the `zetasql::Function`, whose `Name` is the catalog
    /// name (e.g. `$add` for `+`, `lower` for `LOWER()`).
    fn node_function_name(&mut self, node: u64) -> Result<String, Error> {
        let function = self.rpc_handle(SVC_RESOLVED_FUNCTION_CALL_BASE, MID_FUNCTION, node)?;
        self.rpc_string(SVC_FUNCTION, MID_FUNCTION_NAME, function)
    }

    /// Reads the number of value arguments a `ResolvedFunctionCall` takes.
    ///
    /// `argument_list_size()` counts only the value arguments, excluding any
    /// aggregate modifiers. proto3 omits a zero count, so a missing field means
    /// a zero-argument call (e.g. `CURRENT_TIMESTAMP()`); a negative count is
    /// invalid and surfaces as an error rather than being silently clamped.
    fn node_argument_count(&mut self, node: u64) -> Result<usize, Error> {
        let resp = self.invoke(
            SVC_RESOLVED_FUNCTION_CALL_BASE,
            MID_ARGUMENT_LIST_SIZE,
            &pb::handle_arg(node),
        )?;
        check_error(&resp)?;
        let count = pb::read_int32_at_field(&resp, 1).unwrap_or(0);
        usize::try_from(count).map_err(|e| Error::GoogleSql(e.to_string()))
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
