//! High-level API for the GoogleSQL analyzer.
//!
//! Call chain (svc/mid values measured from the wazero reference glue):
//! NewAnalyzerOptions2(554,1) + NewTypeFactory(1419,0) + NewSimpleCatalog(1347,0)
//! → AnalyzeStatement(0,2). All acquired handles are released after use.
//!
//! [`Module::analyze_statement`] resolves against an empty catalog;
//! [`Module::analyze_statement_with_catalog`] registers user-defined tables
//! (via `SimpleTable`/`SimpleColumn`) first. Both report success or failure only.
//! [`Module::analyze_output_columns`] returns the query's resolved output schema,
//! and [`Module::referenced_tables`] returns the tables and columns it reads.
//!
//! All of these share one pipeline: they build the analyzer handles, run the
//! analysis, and let a caller-supplied closure read the `AnalyzerOutput` before
//! the handles are torn down (see [`Module::run_analysis`]).

use crate::error::{Error, check_error};
use crate::pb;
use crate::resolved::{OutputColumn, ResolvedNode, TableRef};
use crate::runtime::{Handle, Module};

const SVC_ANALYZER: i32 = 0;
const MID_ANALYZE_EXPRESSION: i32 = 0;
const MID_ANALYZE_NEXT_STATEMENT: i32 = 1;
const MID_ANALYZE_STATEMENT: i32 = 2;
const MID_ANALYZE_TYPE: i32 = 4;

const SVC_PARSE_RESUME_LOCATION: i32 = 695;
const MID_NEW_RESUME_LOCATION_FROM_STRING: i32 = 2;
const MID_RESUME_LOCATION_BYTE_POSITION: i32 = 10;
const MID_FREE_RESUME_LOCATION: i32 = 14;

const SVC_ANALYZER_OPTIONS: i32 = 554;
const MID_NEW_ANALYZER_OPTIONS: i32 = 1;
const MID_SET_PRUNE_UNUSED_COLUMNS: i32 = 80;
const MID_SET_ALLOW_UNDECLARED_PARAMETERS: i32 = 57;
const MID_ADD_QUERY_PARAMETER: i32 = 4;
const MID_SET_PARSE_LOCATION_RECORD_TYPE: i32 = 77;
const MID_SET_LANGUAGE: i32 = 74;
const MID_FREE_ANALYZER_OPTIONS: i32 = 86;

/// `ParseLocationRecordType::PARSE_LOCATION_RECORD_FULL_NODE_SCOPE`: record a
/// source byte range for every resolved node that maps to one.
const PARSE_LOCATION_RECORD_FULL_NODE_SCOPE: i32 = 2;

const SVC_TYPE_FACTORY: i32 = 1419;
const MID_NEW_TYPE_FACTORY: i32 = 0;
const MID_FREE_TYPE_FACTORY: i32 = 62;
const MID_GET_BIGNUMERIC: i32 = 41;
const MID_GET_BOOL: i32 = 42;
const MID_GET_BYTES: i32 = 43;
const MID_GET_DATE: i32 = 44;
const MID_GET_DATETIME: i32 = 45;
const MID_GET_DOUBLE: i32 = 46;
const MID_GET_GEOGRAPHY: i32 = 48;
const MID_GET_INT64: i32 = 50;
const MID_GET_INTERVAL: i32 = 51;
const MID_GET_JSON: i32 = 52;
const MID_GET_NUMERIC: i32 = 53;
const MID_GET_STRING: i32 = 54;
const MID_GET_TIME: i32 = 55;
const MID_GET_TIMESTAMP: i32 = 56;
const MID_MAKE_ARRAY_TYPE: i32 = 13;
const MID_MAKE_STRUCT_TYPE: i32 = 33;
const MID_MAKE_RANGE_TYPE: i32 = 26;
const MID_MAKE_MAP_TYPE: i32 = 20;
const MID_MAKE_ENUM_TYPE: i32 = 16;
const MID_MAKE_PROTO_TYPE: i32 = 22;

const SVC_SIMPLE_CATALOG: i32 = 1347;
const MID_NEW_SIMPLE_CATALOG: i32 = 0;
const MID_ADD_BUILTIN_FUNCTIONS_AND_TYPES: i32 = 3;
const MID_ADD_FUNCTION: i32 = 10;
const MID_ADD_TABLE_NAMED: i32 = 72;
const MID_ADD_CATALOG_NAMED: i32 = 4;
const MID_ADD_CONNECTION_NAMED: i32 = 6;
const MID_ADD_CONSTANT_NAMED: i32 = 8;
const MID_ADD_TYPE_NAMED: i32 = 75;
const MID_ADD_TABLE_VALUED_FUNCTION_NAMED: i32 = 73;
const MID_ADD_PROPERTY_GRAPH_NAMED: i32 = 68;
const MID_FREE_SIMPLE_CATALOG: i32 = 114;

const SVC_SIMPLE_TABLE: i32 = 1380;
const MID_NEW_SIMPLE_TABLE: i32 = 0;
const MID_ADD_COLUMN_OWNED: i32 = 4;
const MID_FREE_SIMPLE_TABLE: i32 = 27;

const SVC_SIMPLE_COLUMN: i32 = 1350;
const MID_NEW_SIMPLE_COLUMN: i32 = 0;
const MID_FREE_SIMPLE_COLUMN: i32 = 10;

const SVC_SIMPLE_CONNECTION: i32 = 1352;
const MID_NEW_SIMPLE_CONNECTION: i32 = 0;
const MID_FREE_SIMPLE_CONNECTION: i32 = 4;

const SVC_DESCRIPTOR_POOL: i32 = 2;
const MID_NEW_DESCRIPTOR_POOL: i32 = 0;
const MID_BUILD_FILE: i32 = 5;
const MID_FIND_ENUM_TYPE_BY_NAME: i32 = 12;
const MID_FIND_MESSAGE_TYPE_BY_NAME: i32 = 20;
const MID_FREE_DESCRIPTOR_POOL: i32 = 27;

const SVC_FILE_DESCRIPTOR_PROTO: i32 = 7;
const MID_NEW_FILE_DESCRIPTOR_PROTO: i32 = 0;
const MID_FREE_FILE_DESCRIPTOR_PROTO: i32 = 53;

/// `MessageLite` is the protobuf base service; `FileDescriptorProto` inherits it,
/// so a `FileDescriptorProto` handle can be populated from serialized bytes via
/// `ParseFromString` rather than field-by-field setter calls.
const SVC_MESSAGE_LITE: i32 = 10;
const MID_PARSE_FROM_STRING: i32 = 14;

/// Field numbers of a `FileDescriptorProto`: the file name and the repeated
/// `enum_type` (each an `EnumDescriptorProto`).
const FIELD_FDP_NAME: u32 = 1;
const FIELD_FDP_ENUM_TYPE: u32 = 5;

/// Field numbers of an `EnumDescriptorProto`: the enum name and its repeated
/// `value` (each an `EnumValueDescriptorProto`).
const FIELD_ENUM_NAME: u32 = 1;
const FIELD_ENUM_VALUE: u32 = 2;

/// Field numbers of an `EnumValueDescriptorProto`: the value name and number.
const FIELD_ENUM_VALUE_NAME: u32 = 1;
const FIELD_ENUM_VALUE_NUMBER: u32 = 2;

const SVC_FUNCTION_ARGUMENT_TYPE: i32 = 637;
const MID_NEW_FUNCTION_ARGUMENT_TYPE: i32 = 2;
const MID_NEW_FUNCTION_ARGUMENT_TYPE_ANY_RELATION: i32 = 9;
const MID_NEW_FUNCTION_ARGUMENT_TYPE_RELATION_WITH_SCHEMA: i32 = 13;
const MID_FREE_FUNCTION_ARGUMENT_TYPE: i32 = 52;

const SVC_FUNCTION_SIGNATURE: i32 = 644;
const MID_NEW_FUNCTION_SIGNATURE: i32 = 2;
const MID_FREE_FUNCTION_SIGNATURE: i32 = 46;

const SVC_FUNCTION: i32 = 636;
const MID_NEW_FUNCTION: i32 = 3;
const MID_FREE_FUNCTION: i32 = 55;

const SVC_PROCEDURE: i32 = 703;
const MID_NEW_PROCEDURE: i32 = 0;
const MID_FREE_PROCEDURE: i32 = 6;
const MID_ADD_PROCEDURE_NAMED: i32 = 65;

const SVC_TVF_RELATION: i32 = 1400;
const MID_NEW_TVF_RELATION: i32 = 0;
const MID_FREE_TVF_RELATION: i32 = 10;

const SVC_FIXED_OUTPUT_SCHEMA_TVF: i32 = 632;
const MID_NEW_FIXED_OUTPUT_SCHEMA_TVF: i32 = 2;
const MID_FREE_FIXED_OUTPUT_SCHEMA_TVF: i32 = 5;

const SVC_SIMPLE_PROPERTY_GRAPH: i32 = 1375;
const MID_NEW_SIMPLE_PROPERTY_GRAPH: i32 = 0;
const MID_ADD_GRAPH_EDGE_TABLE: i32 = 2;
const MID_ADD_GRAPH_LABEL: i32 = 3;
const MID_ADD_GRAPH_NODE_TABLE: i32 = 4;
const MID_ADD_GRAPH_PROPERTY_DECLARATION: i32 = 5;
const MID_FREE_SIMPLE_PROPERTY_GRAPH: i32 = 14;

const SVC_GRAPH_NODE_TABLE: i32 = 1366;
const MID_NEW_GRAPH_NODE_TABLE: i32 = 0;

const SVC_GRAPH_EDGE_TABLE: i32 = 1360;
const MID_NEW_GRAPH_EDGE_TABLE: i32 = 0;

const SVC_GRAPH_NODE_TABLE_REFERENCE: i32 = 1367;
const MID_NEW_GRAPH_NODE_TABLE_REFERENCE: i32 = 0;

const SVC_GRAPH_ELEMENT_LABEL: i32 = 1363;
const MID_NEW_GRAPH_ELEMENT_LABEL: i32 = 0;

const SVC_GRAPH_PROPERTY_DECLARATION: i32 = 1369;
const MID_NEW_GRAPH_PROPERTY_DECLARATION: i32 = 0;

const SVC_GRAPH_PROPERTY_DEFINITION: i32 = 1371;
const MID_NEW_GRAPH_PROPERTY_DEFINITION: i32 = 0;

/// Field numbers of a `TVFRelation::Column` (`TVFSchemaColumn`) submessage: the
/// column name and its type handle.
const FIELD_TVF_COLUMN_NAME: u32 = 3;
const FIELD_TVF_COLUMN_TYPE: u32 = 5;

const SVC_SIMPLE_CONSTANT: i32 = 1354;
const MID_NEW_SIMPLE_CONSTANT: i32 = 0;
const MID_FREE_SIMPLE_CONSTANT: i32 = 8;

const SVC_VALUE: i32 = 1428;
const MID_NEW_VALUE_BOOL: i32 = 3;
const MID_NEW_VALUE_DOUBLE: i32 = 9;
const MID_NEW_VALUE_INT64: i32 = 18;
const MID_NEW_VALUE_STRING: i32 = 44;
const MID_FREE_VALUE: i32 = 153;

/// `FunctionEnums::Mode::SCALAR` — a plain scalar function.
const FUNCTION_MODE_SCALAR: i32 = 2;
/// `FunctionEnums::Mode::AGGREGATE` — an aggregate function (e.g. `SUM`).
const FUNCTION_MODE_AGGREGATE: i32 = 3;

/// `ResolvedNodeKind` wire values passed to `AddSupportedStatementKind` to
/// restrict which statement kinds the analyzer resolves (see [`StatementKind`]).
const NODE_KIND_QUERY_STMT: i32 = 39;
const NODE_KIND_CREATE_TABLE_AS_SELECT_STMT: i32 = 41;
const NODE_KIND_CREATE_VIEW_STMT: i32 = 42;
const NODE_KIND_CREATE_EXTERNAL_TABLE_STMT: i32 = 43;
const NODE_KIND_INSERT_STMT: i32 = 64;
const NODE_KIND_DELETE_STMT: i32 = 65;
const NODE_KIND_UPDATE_STMT: i32 = 67;
const NODE_KIND_CREATE_ROW_ACCESS_POLICY_STMT: i32 = 74;
const NODE_KIND_CREATE_FUNCTION_STMT: i32 = 77;
const NODE_KIND_CREATE_TABLE_STMT: i32 = 91;
const NODE_KIND_MERGE_STMT: i32 = 102;
const NODE_KIND_CREATE_MATERIALIZED_VIEW_STMT: i32 = 120;

/// `LanguageFeature` wire values passed to `DisableLanguageFeature` to turn a
/// feature off (see [`LanguageFeature`]).
const FEATURE_ANALYTIC_FUNCTIONS: i32 = 2;
const FEATURE_TABLESAMPLE: i32 = 3;
const FEATURE_NUMERIC_TYPE: i32 = 17;
const FEATURE_GEOGRAPHY: i32 = 26;
const FEATURE_NAMED_ARGUMENTS: i32 = 32;
const FEATURE_JSON_TYPE: i32 = 44;
const FEATURE_INTERVAL_TYPE: i32 = 50;
const FEATURE_QUALIFY: i32 = 13034;
/// `num_occurrences` for a required argument in a signature (exactly one).
const ARGUMENT_REQUIRED_OCCURRENCES: i32 = 1;
/// Group name assigned to functions registered through [`Catalog::functions`].
const USER_FUNCTION_GROUP: &str = "UDF";

/// Field of `BuiltinFunctionOptions` that carries the `LanguageOptions` handle.
const FIELD_BUILTIN_OPTIONS_LANGUAGE: u32 = 4;

const SVC_ANALYZER_OUTPUT: i32 = 558;
const MID_FREE_ANALYZER_OUTPUT: i32 = 11;

/// A column type that a user table can expose to the analyzer.
///
/// Each scalar variant maps to a `TypeFactory` accessor for the corresponding
/// GoogleSQL type; [`Array`](Self::Array) wraps an element type, built
/// recursively (so `ARRAY<STRING>` is `Array(Box::new(String))`).
///
/// Marked `#[non_exhaustive]` because GoogleSQL has more types than are modelled
/// here; adding a variant later must not be a breaking change for callers that
/// match on it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColumnType {
    /// 64-bit signed integer (`INT64`).
    Int64,
    /// Variable-length string (`STRING`).
    String,
    /// Boolean (`BOOL`).
    Bool,
    /// Double-precision float (`FLOAT64`).
    Float64,
    /// Variable-length bytes (`BYTES`).
    Bytes,
    /// Calendar date (`DATE`).
    Date,
    /// Date and time with no time zone (`DATETIME`).
    Datetime,
    /// Time of day (`TIME`).
    Time,
    /// Absolute point in time (`TIMESTAMP`).
    Timestamp,
    /// Exact decimal with fixed precision and scale (`NUMERIC`).
    Numeric,
    /// Exact decimal with extended precision and scale (`BIGNUMERIC`).
    BigNumeric,
    /// JSON value (`JSON`).
    Json,
    /// Duration between two points in time (`INTERVAL`).
    Interval,
    /// Geospatial value (`GEOGRAPHY`).
    Geography,
    /// An array of a given element type (`ARRAY<T>`). GoogleSQL forbids arrays of
    /// arrays, so a nested `Array` element is rejected during analysis.
    Array(Box<Self>),
    /// A struct with named, typed fields (`STRUCT<name T, ...>`), in order.
    Struct(Vec<StructField>),
    /// A contiguous range over an ordered element type (`RANGE<T>`). GoogleSQL
    /// allows only `DATE`, `DATETIME`, or `TIMESTAMP` elements; any other element
    /// is rejected during analysis.
    Range(Box<Self>),
    /// A map from a key type to a value type (`MAP<K, V>`). The key type must
    /// support grouping; the value type is unrestricted.
    Map(Box<Self>, Box<Self>),
    /// A protobuf message type, by its full name (e.g. `my.package.Person`). The
    /// message must be declared in one of the catalog's
    /// [`proto_files`](Catalog::proto_files); it is looked up in the descriptor
    /// pool built from them.
    Proto(String),
}

/// A named field of a [`ColumnType::Struct`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructField {
    /// The field name as referenced in SQL (e.g. `s.x`).
    pub name: String,
    /// The field's type.
    pub ty: ColumnType,
}

impl ColumnType {
    /// The `TypeFactory` getter method id for a scalar type, or `None` for a
    /// constructed type (e.g. [`Array`](Self::Array)) that needs a builder.
    const fn scalar_type_factory_mid(&self) -> Option<i32> {
        Some(match self {
            Self::Int64 => MID_GET_INT64,
            Self::String => MID_GET_STRING,
            Self::Bool => MID_GET_BOOL,
            Self::Float64 => MID_GET_DOUBLE,
            Self::Bytes => MID_GET_BYTES,
            Self::Date => MID_GET_DATE,
            Self::Datetime => MID_GET_DATETIME,
            Self::Time => MID_GET_TIME,
            Self::Timestamp => MID_GET_TIMESTAMP,
            Self::Numeric => MID_GET_NUMERIC,
            Self::BigNumeric => MID_GET_BIGNUMERIC,
            Self::Json => MID_GET_JSON,
            Self::Interval => MID_GET_INTERVAL,
            Self::Geography => MID_GET_GEOGRAPHY,
            Self::Array(_)
            | Self::Struct(_)
            | Self::Range(_)
            | Self::Map(_, _)
            | Self::Proto(_) => return None,
        })
    }
}

/// A column of a user-defined table: its name and type.
#[derive(Debug, Clone)]
pub struct ColumnDef {
    /// The column name as referenced in SQL.
    pub name: String,
    /// The column's type.
    pub ty: ColumnType,
}

/// A user-defined table registered into the catalog before analysis.
#[derive(Debug, Clone)]
pub struct TableDef {
    /// The table name as referenced in SQL.
    pub name: String,
    /// The table's columns, in declaration order.
    pub columns: Vec<ColumnDef>,
}

/// A user-defined function (scalar or aggregate) registered into the catalog
/// before analysis.
///
/// Registering it lets a call such as `my_add(1, 2)` resolve, type-checked
/// against the declared argument types and yielding the declared return type.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    /// The function name as called in SQL.
    pub name: String,
    /// The argument types, in order; each call argument is checked against these.
    pub arguments: Vec<ColumnType>,
    /// The type the function returns.
    pub return_type: ColumnType,
    /// Whether the function is scalar or aggregate; controls how calls to it may
    /// be written (an aggregate must be called with aggregate semantics).
    pub kind: FunctionKind,
}

/// How a registered [`FunctionDef`] is invoked in SQL.
///
/// Marked `#[non_exhaustive]` because GoogleSQL supports more modes (e.g.
/// analytic/window functions) than are modeled here yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FunctionKind {
    /// A plain scalar function, called once per row (e.g. `LOWER(x)`).
    #[default]
    Scalar,
    /// An aggregate function, called over a group of rows (e.g. `SUM(x)`).
    Aggregate,
}

impl FunctionKind {
    /// The `FunctionEnums::Mode` wire value passed to `NewFunction`.
    const fn mode(self) -> i32 {
        match self {
            Self::Scalar => FUNCTION_MODE_SCALAR,
            Self::Aggregate => FUNCTION_MODE_AGGREGATE,
        }
    }
}

/// A category of statement the analyzer may be restricted to accept, selected
/// with [`Module::set_supported_statement_kinds`].
///
/// By default the analyzer resolves every kind. Restricting to a set is useful
/// for validation, e.g. allowing only [`Query`](Self::Query) to reject any DML
/// or DDL a caller must not run.
///
/// Marked `#[non_exhaustive]` because GoogleSQL has more statement kinds than
/// are modelled here; adding a variant later must not break callers that match
/// on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StatementKind {
    /// A query statement (`SELECT`).
    Query,
    /// An `INSERT` statement.
    Insert,
    /// An `UPDATE` statement.
    Update,
    /// A `DELETE` statement.
    Delete,
    /// A `MERGE` statement.
    Merge,
    /// A `CREATE TABLE` statement.
    CreateTable,
    /// A `CREATE TABLE ... AS SELECT` statement.
    CreateTableAsSelect,
    /// A `CREATE VIEW` statement.
    CreateView,
    /// A `CREATE MATERIALIZED VIEW` statement.
    CreateMaterializedView,
    /// A `CREATE EXTERNAL TABLE` statement.
    CreateExternalTable,
    /// A `CREATE FUNCTION` statement.
    CreateFunction,
    /// A `CREATE ROW ACCESS POLICY` statement.
    CreateRowAccessPolicy,
}

impl StatementKind {
    /// The `ResolvedNodeKind` wire value passed to `AddSupportedStatementKind`.
    const fn node_kind(self) -> i32 {
        match self {
            Self::Query => NODE_KIND_QUERY_STMT,
            Self::Insert => NODE_KIND_INSERT_STMT,
            Self::Update => NODE_KIND_UPDATE_STMT,
            Self::Delete => NODE_KIND_DELETE_STMT,
            Self::Merge => NODE_KIND_MERGE_STMT,
            Self::CreateTable => NODE_KIND_CREATE_TABLE_STMT,
            Self::CreateTableAsSelect => NODE_KIND_CREATE_TABLE_AS_SELECT_STMT,
            Self::CreateView => NODE_KIND_CREATE_VIEW_STMT,
            Self::CreateMaterializedView => NODE_KIND_CREATE_MATERIALIZED_VIEW_STMT,
            Self::CreateExternalTable => NODE_KIND_CREATE_EXTERNAL_TABLE_STMT,
            Self::CreateFunction => NODE_KIND_CREATE_FUNCTION_STMT,
            Self::CreateRowAccessPolicy => NODE_KIND_CREATE_ROW_ACCESS_POLICY_STMT,
        }
    }
}

/// A GoogleSQL language feature that can be turned off with
/// [`Module::disable_language_features`].
///
/// The analyzer enables the maximum feature set by default, so every feature
/// here is normally on. Disabling one lets a caller enforce a stricter dialect
/// subset — e.g. disabling [`Qualify`](Self::Qualify) rejects the `QUALIFY`
/// clause, or [`AnalyticFunctions`](Self::AnalyticFunctions) rejects window
/// functions.
///
/// Marked `#[non_exhaustive]` because GoogleSQL has many more features than are
/// modelled here; adding a variant later must not break callers that match on
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LanguageFeature {
    /// Analytic (window) functions, i.e. calls with an `OVER` clause.
    AnalyticFunctions,
    /// The `TABLESAMPLE` clause.
    Tablesample,
    /// The `NUMERIC` type.
    NumericType,
    /// The `GEOGRAPHY` type.
    Geography,
    /// Named arguments in function calls (`f(name => value)`).
    NamedArguments,
    /// The `JSON` type.
    JsonType,
    /// The `INTERVAL` type.
    IntervalType,
    /// The `QUALIFY` clause.
    Qualify,
}

impl LanguageFeature {
    /// The `LanguageFeature` wire value passed to `DisableLanguageFeature`.
    const fn feature_id(self) -> i32 {
        match self {
            Self::AnalyticFunctions => FEATURE_ANALYTIC_FUNCTIONS,
            Self::Tablesample => FEATURE_TABLESAMPLE,
            Self::NumericType => FEATURE_NUMERIC_TYPE,
            Self::Geography => FEATURE_GEOGRAPHY,
            Self::NamedArguments => FEATURE_NAMED_ARGUMENTS,
            Self::JsonType => FEATURE_JSON_TYPE,
            Self::IntervalType => FEATURE_INTERVAL_TYPE,
            Self::Qualify => FEATURE_QUALIFY,
        }
    }
}

/// A named constant registered into the catalog before analysis.
///
/// Registering it lets the bare name resolve as an expression yielding the
/// constant's value and type, e.g. `SELECT my_const`.
#[derive(Debug, Clone)]
pub struct ConstantDef {
    /// The constant name as referenced in SQL.
    pub name: String,
    /// The constant's value, which also determines its type.
    pub value: ConstantValue,
}

/// The value (and thus type) of a registered [`ConstantDef`].
///
/// Marked `#[non_exhaustive]` because GoogleSQL has more value types than the
/// scalars modeled here.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ConstantValue {
    /// An `INT64` constant.
    Int64(i64),
    /// A `DOUBLE` constant.
    Double(f64),
    /// A `BOOL` constant.
    Bool(bool),
    /// A `STRING` constant.
    String(String),
}

/// A typed query parameter declared for the analysis.
///
/// Declaring `@id` as `INT64` lets `SELECT @id` resolve to a known type and
/// type-checks every use of the parameter against it. Without any declaration a
/// `@param` still resolves (its type is inferred from context).
///
/// Declaring any parameter switches the analysis to strict mode: every other
/// `@param` in the query must then be declared too, or analysis fails. So either
/// declare no parameters (all inferred) or declare all of them.
#[derive(Debug, Clone)]
pub struct QueryParameter {
    /// The parameter name, without the leading `@`.
    pub name: String,
    /// The parameter's type.
    pub ty: ColumnType,
}

/// An argument accepted by a table-valued function (see [`TvfDef::arguments`]).
///
/// Marked `#[non_exhaustive]` because GoogleSQL supports more argument kinds
/// (e.g. model or connection arguments) than are modelled here; adding a variant
/// later must not break callers that match on it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TvfArgument {
    /// A scalar argument of the given type, e.g. `my_tvf(5)` for
    /// `Scalar(ColumnType::Int64)`.
    Scalar(ColumnType),
    /// A relation (table) argument accepting any input schema, e.g.
    /// `my_tvf(TABLE t)` for any table `t`.
    AnyRelation,
    /// A relation (table) argument whose input must provide exactly these
    /// columns (extra columns are not allowed), e.g. a table matching
    /// `(a INT64)`.
    Relation(Vec<ColumnDef>),
}

/// A user-defined table-valued function (TVF) with a fixed output schema,
/// registered into the catalog before analysis.
///
/// Registering it lets a call such as `SELECT * FROM my_tvf(5)` resolve,
/// type-checking each argument against [`arguments`](Self::arguments) and
/// yielding the declared output columns. The output schema is fixed: it does
/// not depend on the argument values.
#[derive(Debug, Clone)]
pub struct TvfDef {
    /// The function name as called in SQL.
    pub name: String,
    /// The arguments, in order; each call argument is checked against these.
    /// Empty for a function called as `my_tvf()`.
    pub arguments: Vec<TvfArgument>,
    /// The columns the function outputs, in order; each becomes a column of the
    /// relation the call produces.
    pub columns: Vec<ColumnDef>,
}

/// A user-defined stored procedure, registered into the catalog before
/// analysis.
///
/// Registering it lets a `CALL my_proc(1)` statement resolve, type-checking each
/// call argument against [`arguments`](Self::arguments). A procedure has no
/// expression result — it is invoked by `CALL`, not referenced in a query.
#[derive(Debug, Clone)]
pub struct ProcedureDef {
    /// The procedure name as invoked by `CALL`.
    pub name: String,
    /// The argument types, in order; each `CALL` argument is checked against
    /// these. Empty for a procedure called as `CALL my_proc()`.
    pub arguments: Vec<ColumnType>,
}

/// A single value of a user-defined [`EnumDef`]: its name and integer number.
#[derive(Debug, Clone)]
pub struct EnumValue {
    /// The value name as referenced in SQL (e.g. `'RED'` in `CAST('RED' AS Color)`).
    pub name: String,
    /// The value's integer number.
    pub number: i32,
}

/// A user-defined enum type registered into the catalog before analysis.
///
/// Registering it makes [`name`](Self::name) usable wherever a type name is
/// expected (e.g. `CAST(x AS Color)`); casting a string literal to the enum
/// resolves only against the declared [`values`](Self::values). Internally the
/// enum is materialized as a protobuf `EnumDescriptor` built into a descriptor
/// pool, mirroring how GoogleSQL represents enum types.
#[derive(Debug, Clone)]
pub struct EnumDef {
    /// The enum type name as referenced in SQL.
    pub name: String,
    /// The enum's values.
    pub values: Vec<EnumValue>,
}

/// A user-defined named type registered into the catalog before analysis.
///
/// Registering it makes the name usable wherever a type name is expected, e.g.
/// `CAST(x AS my_type)` or a typed `NULL`. The name resolves to its underlying
/// [`ty`](Self::ty); it is an alias, so the resolved type is the underlying type
/// (a `NamedType` mapping `point` to `STRUCT<x FLOAT64, y FLOAT64>` resolves to
/// that struct).
#[derive(Debug, Clone)]
pub struct NamedType {
    /// The type name as referenced in SQL.
    pub name: String,
    /// The underlying type the name resolves to.
    pub ty: ColumnType,
}

/// A user-defined property graph registered into the catalog before analysis.
///
/// Registering it lets a graph query resolve against it, e.g.
/// `GRAPH people MATCH (n:Person) RETURN n.name`. A property graph is a set of
/// node (vertex) tables and edge tables, each backed by an input table schema
/// and exposing typed properties grouped under labels.
#[derive(Debug, Clone)]
pub struct PropertyGraphDef {
    /// The graph name, as referenced by `GRAPH <name>`.
    pub name: String,
    /// The node (vertex) tables that make up the graph.
    pub node_tables: Vec<GraphNodeTableDef>,
    /// The edge tables connecting node tables, each carrying a source and a
    /// destination reference into a [`node_tables`](Self::node_tables) entry.
    pub edge_tables: Vec<GraphEdgeTableDef>,
}

/// An edge table within a [`PropertyGraphDef`], connecting a source node to a
/// destination node.
///
/// Like a [`GraphNodeTableDef`] it is backed by an inline input schema and
/// exposes properties through labels, and additionally carries a
/// [`source`](Self::source) and [`destination`](Self::destination) reference
/// pinning each edge to the nodes it connects.
#[derive(Debug, Clone)]
pub struct GraphEdgeTableDef {
    /// The element table name, as referenced by a graph pattern.
    pub name: String,
    /// The input table schema backing the edge table.
    pub columns: Vec<ColumnDef>,
    /// Indices into [`columns`](Self::columns) forming each edge's key.
    pub key_columns: Vec<u32>,
    /// The labels exposed by the edge table, each declaring a set of properties.
    ///
    /// A property name must be declared at most once across the edge table's
    /// labels; GoogleSQL rejects a graph that declares the same property twice.
    pub labels: Vec<GraphLabelDef>,
    /// The source (tail) node the edge starts from.
    pub source: GraphNodeReferenceDef,
    /// The destination (head) node the edge points to.
    pub destination: GraphNodeReferenceDef,
}

/// A reference from a [`GraphEdgeTableDef`] to one of its graph's node tables,
/// pinning an edge endpoint to a node by matching columns.
///
/// The edge's [`edge_columns`](Self::edge_columns) (indices into the edge
/// table's input schema) are matched against the referenced node table's
/// [`node_columns`](Self::node_columns) (indices into that node table's input
/// schema), the way a foreign key matches a primary key.
#[derive(Debug, Clone)]
pub struct GraphNodeReferenceDef {
    /// The name of a node table in the same [`PropertyGraphDef`].
    pub node_table: String,
    /// Indices into the edge table's input columns forming the endpoint key.
    pub edge_columns: Vec<u32>,
    /// Indices into the referenced node table's input columns matched against
    /// [`edge_columns`](Self::edge_columns).
    pub node_columns: Vec<u32>,
}

/// A node (vertex) table within a [`PropertyGraphDef`].
///
/// Backed by an inline input schema ([`columns`](Self::columns)); each node's
/// identity is its [`key_columns`](Self::key_columns), and its properties are
/// exposed through [`labels`](Self::labels).
#[derive(Debug, Clone)]
pub struct GraphNodeTableDef {
    /// The element table name, as referenced by a graph pattern.
    pub name: String,
    /// The input table schema backing the node table; property value
    /// expressions ([`GraphPropertyDef::value_sql`]) are evaluated over these
    /// columns.
    pub columns: Vec<ColumnDef>,
    /// Indices into [`columns`](Self::columns) forming each node's key.
    pub key_columns: Vec<u32>,
    /// The labels exposed by the node table, each declaring a set of properties.
    ///
    /// A property name must be declared at most once across the node table's
    /// labels; GoogleSQL rejects a graph that declares the same property twice.
    pub labels: Vec<GraphLabelDef>,
}

/// A label exposed by a [`GraphNodeTableDef`], grouping a set of properties.
///
/// A graph pattern selects elements by label, e.g. the `Person` in
/// `MATCH (n:Person)`.
#[derive(Debug, Clone)]
pub struct GraphLabelDef {
    /// The label name, as used in a graph pattern (`(n:Person)`).
    pub name: String,
    /// The properties elements carrying this label expose.
    pub properties: Vec<GraphPropertyDef>,
}

/// A property exposed under a [`GraphLabelDef`].
///
/// A property has a [`name`](Self::name) and [`ty`](Self::ty); its value is the
/// SQL expression [`value_sql`](Self::value_sql) evaluated over the owning node
/// table's input [`columns`](GraphNodeTableDef::columns) — e.g. a property
/// `name STRING` with `value_sql: "full_name"`.
#[derive(Debug, Clone)]
pub struct GraphPropertyDef {
    /// The property name, as accessed by `element.name`.
    pub name: String,
    /// The property type.
    pub ty: ColumnType,
    /// The SQL expression, over the node table's input columns, giving the value.
    pub value_sql: String,
}

/// The declarations made visible to the analyzer for a query.
///
/// Bundles the [`tables`](Self::tables), [`functions`](Self::functions),
/// [`constants`](Self::constants), [`parameters`](Self::parameters),
/// [`table_functions`](Self::table_functions), [`procedures`](Self::procedures),
/// [`connections`](Self::connections), [`types`](Self::types), and nested
/// [`catalogs`](Self::catalogs) a query may reference, passed to the `*_in`
/// analysis entry points. The table-only entry points (e.g.
/// [`Module::analyze_output_columns`]) are equivalent to a `Catalog` with only
/// tables.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// User-defined tables.
    pub tables: Vec<TableDef>,
    /// User-defined functions.
    pub functions: Vec<FunctionDef>,
    /// User-defined named constants.
    pub constants: Vec<ConstantDef>,
    /// Typed query parameters (`@name`).
    ///
    /// Only honored on the top-level catalog: parameters are an analysis-wide
    /// setting, not a namespaced declaration, so any set on a nested
    /// [`NamedCatalog`] are ignored.
    pub parameters: Vec<QueryParameter>,
    /// User-defined table-valued functions.
    pub table_functions: Vec<TvfDef>,
    /// Nested sub-catalogs, each a named namespace whose declarations resolve
    /// only under its qualified name (e.g. `dataset.table`).
    pub catalogs: Vec<NamedCatalog>,
    /// User-defined stored procedures, invoked by `CALL`.
    pub procedures: Vec<ProcedureDef>,
    /// User-defined external connections, by name. Registering a connection lets
    /// a statement that references it resolve, e.g.
    /// `CREATE EXTERNAL TABLE ... WITH CONNECTION my_conn`.
    pub connections: Vec<String>,
    /// User-defined named types, usable wherever a type name is expected (e.g.
    /// `CAST(x AS my_type)`).
    pub types: Vec<NamedType>,
    /// User-defined enum types, usable wherever a type name is expected (e.g.
    /// `CAST(x AS Color)`).
    pub enums: Vec<EnumDef>,
    /// Serialized `FileDescriptorProto`s describing protobuf message and enum
    /// types. Each entry is the wire encoding of a single `FileDescriptorProto`
    /// (e.g. from `protoc --descriptor_set_out` or a protobuf library); they are
    /// built into a shared descriptor pool so a [`ColumnType::Proto`] can resolve
    /// its message by full name.
    ///
    /// Only honored on the top-level catalog, like [`parameters`](Self::parameters).
    pub proto_files: Vec<Vec<u8>>,
    /// User-defined property graphs, resolvable by a graph query (e.g.
    /// `GRAPH people MATCH (n:Person) RETURN n.name`).
    pub property_graphs: Vec<PropertyGraphDef>,
}

/// A named nested catalog: a namespace whose declarations resolve only under
/// its name.
///
/// Registering a `NamedCatalog` called `ds` whose inner [`Catalog`] holds a
/// table `t` lets `SELECT * FROM ds.t` resolve, mirroring a BigQuery
/// `dataset.table`. The unqualified `t` stays unresolved: nesting does not
/// flatten the namespace. Sub-catalogs may nest arbitrarily deep (`a.b.t`).
///
/// Unlike the top-level catalog, a nested catalog does not receive GoogleSQL's
/// builtin functions and types — it is a pure namespace for user declarations.
#[derive(Debug, Clone)]
pub struct NamedCatalog {
    /// The namespace name, as used to qualify references (the `ds` in `ds.t`).
    pub name: String,
    /// The declarations visible under [`name`](Self::name).
    pub catalog: Catalog,
}

/// Per-analysis toggles applied to the `AnalyzerOptions` before resolving.
///
/// Each entry point requests only what it needs so the others stay unaffected:
/// column pruning for [`Module::referenced_tables`], parse-location recording
/// for [`Module::resolved_tree`].
#[derive(Clone, Copy, Default)]
struct AnalysisOptions {
    /// Expose only the columns the query references on each table scan.
    prune_columns: bool,
    /// Record a source byte range on each resolved node.
    record_parse_locations: bool,
    /// Which analyzer entry point to invoke on the input string.
    target: AnalysisTarget,
}

/// Selects the analyzer entry point for a run: a full statement, a standalone
/// scalar expression, or a type name. The default is [`AnalysisTarget::Statement`],
/// so the raw zero-valued default never accidentally selects `AnalyzeExpression`
/// (`mid` 0).
#[derive(Clone, Copy, Default)]
enum AnalysisTarget {
    /// `AnalyzeStatement`: the input is a complete SQL statement.
    #[default]
    Statement,
    /// `AnalyzeExpression`: the input is a scalar expression, resolved to a
    /// single typed `ResolvedExpr`.
    Expression,
    /// `AnalyzeType`: the input is a type name, resolved to a `Type`.
    Type,
}

impl AnalysisTarget {
    /// The analyzer service method id (`SVC_ANALYZER`) this target invokes.
    const fn mid(self) -> i32 {
        match self {
            Self::Statement => MID_ANALYZE_STATEMENT,
            Self::Expression => MID_ANALYZE_EXPRESSION,
            Self::Type => MID_ANALYZE_TYPE,
        }
    }

    /// Whether the field-2 handle in the response is an owned `AnalyzerOutput`
    /// that must be freed. `AnalyzeStatement`/`AnalyzeExpression` return one;
    /// `AnalyzeType` returns a `Type` borrowed from the type factory (freed with
    /// it), so its handle must not be freed here.
    const fn frees_output(self) -> bool {
        match self {
            Self::Statement | Self::Expression => true,
            Self::Type => false,
        }
    }
}

/// One analyzer invocation threaded through the analysis pipeline: the configured
/// `AnalyzerOptions` handle, the `SVC_ANALYZER` method id selecting the entry
/// point, and whether its output handle is owned. Bundled so the shared helpers
/// carry a single value rather than the trio.
#[derive(Clone, Copy)]
struct AnalyzerCall {
    /// Handle to the configured `AnalyzerOptions`.
    options: u64,
    /// `SVC_ANALYZER` method id: e.g. `MID_ANALYZE_STATEMENT` or `MID_ANALYZE_TYPE`.
    mid: i32,
    /// Whether the response's field-2 handle is an owned `AnalyzerOutput` to free.
    frees_output: bool,
}

/// The catalog entries to register before an analysis, threaded together through
/// the pipeline so the table-only and [`Catalog`] entry points share one path.
#[derive(Clone, Copy)]
struct CatalogContents<'a> {
    tables: &'a [TableDef],
    functions: &'a [FunctionDef],
    constants: &'a [ConstantDef],
    parameters: &'a [QueryParameter],
    table_functions: &'a [TvfDef],
    catalogs: &'a [NamedCatalog],
    procedures: &'a [ProcedureDef],
    connections: &'a [String],
    types: &'a [NamedType],
    enums: &'a [EnumDef],
    proto_files: &'a [Vec<u8>],
    property_graphs: &'a [PropertyGraphDef],
}

impl<'a> CatalogContents<'a> {
    /// Only tables, no functions, constants, parameters, or sub-catalogs (the
    /// table-only entry points).
    const fn tables_only(tables: &'a [TableDef]) -> Self {
        Self {
            tables,
            functions: &[],
            constants: &[],
            parameters: &[],
            table_functions: &[],
            catalogs: &[],
            procedures: &[],
            connections: &[],
            types: &[],
            enums: &[],
            proto_files: &[],
            property_graphs: &[],
        }
    }

    /// Every kind of declaration made by a [`Catalog`].
    const fn of(catalog: &'a Catalog) -> Self {
        Self {
            tables: catalog.tables.as_slice(),
            functions: catalog.functions.as_slice(),
            constants: catalog.constants.as_slice(),
            parameters: catalog.parameters.as_slice(),
            table_functions: catalog.table_functions.as_slice(),
            catalogs: catalog.catalogs.as_slice(),
            procedures: catalog.procedures.as_slice(),
            connections: catalog.connections.as_slice(),
            types: catalog.types.as_slice(),
            enums: catalog.enums.as_slice(),
            proto_files: catalog.proto_files.as_slice(),
            property_graphs: catalog.property_graphs.as_slice(),
        }
    }
}

/// Wire-encodes an [`EnumDef`] as a `FileDescriptorProto` declaring a single
/// enum. Each file name is made unique per `index` so several enums can share
/// one descriptor pool without name collisions. The value number is encoded as
/// a sign-extended varint (protobuf's `int32`) so negative numbers round-trip.
fn encode_enum_fdp(def: &EnumDef, index: usize) -> Vec<u8> {
    let mut enum_type = Vec::new();
    pb::append_string(&mut enum_type, FIELD_ENUM_NAME, &def.name);
    for value in &def.values {
        let mut value_msg = Vec::new();
        pb::append_string(&mut value_msg, FIELD_ENUM_VALUE_NAME, &value.name);
        pb::append_int64(
            &mut value_msg,
            FIELD_ENUM_VALUE_NUMBER,
            i64::from(value.number),
        );
        pb::append_submessage(&mut enum_type, FIELD_ENUM_VALUE, &value_msg);
    }

    let mut fdp = Vec::new();
    pb::append_string(
        &mut fdp,
        FIELD_FDP_NAME,
        &format!("googlesql_enum_{index}.proto"),
    );
    pb::append_submessage(&mut fdp, FIELD_FDP_ENUM_TYPE, &enum_type);
    fdp
}

/// Appends each `u32` column index in `indices` as a repeated `int32` at
/// `field`, failing if one exceeds `i32::MAX` (indices are always small, so this
/// is a guard rather than an expected case). Shared by the graph element table
/// and node reference builders, which all carry column indices this way.
fn append_column_indices(req: &mut Vec<u8>, field: u32, indices: &[u32]) -> Result<(), Error> {
    for &index in indices {
        let value = i32::try_from(index)
            .map_err(|_| Error::Protocol("graph column index out of range".into()))?;
        pb::append_int32(req, field, value);
    }
    Ok(())
}

impl Module {
    /// Restricts analysis to the given statement kinds; other kinds then fail
    /// with a "Statement not supported" error.
    ///
    /// The default (and the effect of passing an empty slice, mirroring ZetaSQL's
    /// `SetSupportedStatementKinds({})`) is to accept every kind. Restricting to
    /// `&[StatementKind::Query]`, for example, lets `SELECT` resolve while
    /// rejecting any DML or DDL. The restriction applies to every subsequent
    /// analysis on this [`Module`].
    pub fn set_supported_statement_kinds(&mut self, kinds: &[StatementKind]) {
        self.set_supported_statement_kinds_raw(kinds.iter().map(|k| k.node_kind()).collect());
    }

    /// Turns off the given language features, which are otherwise all enabled.
    ///
    /// The analyzer enables the maximum feature set by default; each feature
    /// passed here is disabled on top of that, so syntax gated behind it then
    /// fails to resolve (e.g. disabling [`LanguageFeature::Qualify`] rejects the
    /// `QUALIFY` clause). Passing an empty slice disables nothing, restoring the
    /// default. The setting applies to every subsequent analysis on this
    /// [`Module`].
    pub fn disable_language_features(&mut self, features: &[LanguageFeature]) {
        self.set_disabled_language_features_raw(features.iter().map(|f| f.feature_id()).collect());
    }

    /// Enables only the given language features, starting from a minimal set with
    /// every optional feature turned off.
    ///
    /// This is the mirror image of [`disable_language_features`](Self::disable_language_features):
    /// instead of the maximum feature set minus a few, the analyzer begins with
    /// nothing optional enabled and turns on only the features passed here, so any
    /// syntax gated behind an unlisted feature fails to resolve. Passing an empty
    /// slice leaves every optional feature off. Calling this replaces any earlier
    /// [`disable_language_features`](Self::disable_language_features) choice, and
    /// the setting applies to every subsequent analysis on this [`Module`].
    pub fn enable_only_language_features(&mut self, features: &[LanguageFeature]) {
        self.set_enabled_language_features_raw(features.iter().map(|f| f.feature_id()).collect());
    }

    /// Analyzes a SQL statement against an empty catalog.
    ///
    /// Performs type inference and name resolution. Returns [`Error::GoogleSql`]
    /// on a syntax error or when a referenced name cannot be resolved (which, with
    /// an empty catalog, includes any table reference).
    pub fn analyze_statement(&mut self, sql: &str) -> Result<(), Error> {
        self.analyze_statement_with_catalog(sql, &[])
    }

    /// Analyzes a SQL statement against a catalog populated with `tables`.
    ///
    /// Each table (and its columns) is registered as a `SimpleTable` before
    /// analysis, so queries referencing those tables resolve. Passing an empty
    /// slice is equivalent to [`Module::analyze_statement`]. Returns
    /// [`Error::GoogleSql`] on a syntax error or unresolved name.
    pub fn analyze_statement_with_catalog(
        &mut self,
        sql: &str,
        tables: &[TableDef],
    ) -> Result<(), Error> {
        self.run_analysis(
            sql,
            CatalogContents::tables_only(tables),
            AnalysisOptions::default(),
            |_, _| Ok(()),
        )
    }

    /// Analyzes a standalone scalar expression and returns its inferred type.
    ///
    /// Unlike [`Module::analyze_statement`], the input is an expression (not a
    /// full statement); it resolves against a catalog populated with `tables`
    /// and the builtin functions/operators, and the returned string is the
    /// resolved type's name (e.g. `"INT64"`, `"STRING"`, `"STRUCT<a INT64>"`).
    /// The expression may reference catalog constants and functions but not table
    /// columns, which are not in scope. Returns [`Error::GoogleSql`] on a syntax
    /// error or unresolved name.
    pub fn analyze_expression(&mut self, sql: &str, tables: &[TableDef]) -> Result<String, Error> {
        self.run_analysis(
            sql,
            CatalogContents::tables_only(tables),
            AnalysisOptions {
                target: AnalysisTarget::Expression,
                ..AnalysisOptions::default()
            },
            Self::expression_type,
        )
    }

    /// Analyzes every statement in a possibly multi-statement `sql` against a
    /// catalog populated with `tables`, returning how many statements were
    /// analyzed.
    ///
    /// Statements are separated by semicolons (a trailing semicolon is optional);
    /// each is resolved in turn against the same catalog. Analysis stops at the
    /// first statement that fails, returning [`Error::GoogleSql`] with a location
    /// relative to the whole script. An empty or whitespace-only script yields
    /// `0`.
    pub fn analyze_statements(&mut self, sql: &str, tables: &[TableDef]) -> Result<usize, Error> {
        let analyzed = self.run_multi_analysis(
            sql,
            CatalogContents::tables_only(tables),
            AnalysisOptions::default(),
            |_, _| Ok(()),
        )?;
        Ok(analyzed.len())
    }

    /// Resolves a type name to its canonical resolved type name.
    ///
    /// Parses and analyzes `type_name` (e.g. `"INT64"`, `"ARRAY<STRING>"`,
    /// `"STRUCT<a INT64, b STRING>"`) against a catalog populated with `tables`,
    /// returning the resolved type's name. Structural types round-trip; catalog
    /// entries here only matter for named types, so [`Module::analyze_type_in`] is
    /// the useful counterpart when resolving catalog-defined type names. Returns
    /// [`Error::GoogleSql`] for a malformed or unknown type name.
    pub fn analyze_type(&mut self, type_name: &str, tables: &[TableDef]) -> Result<String, Error> {
        self.run_analysis(
            type_name,
            CatalogContents::tables_only(tables),
            AnalysisOptions {
                target: AnalysisTarget::Type,
                ..AnalysisOptions::default()
            },
            Self::resolved_type_name,
        )
    }

    /// Analyzes a query and returns its resolved output schema.
    ///
    /// Like [`Module::analyze_statement_with_catalog`], but instead of only
    /// reporting success it returns one [`OutputColumn`] per column the query
    /// produces, each carrying the column's name and resolved type. Non-query
    /// statements have no output schema and yield an empty vec. Returns
    /// [`Error::GoogleSql`] on a syntax error or unresolved name.
    pub fn analyze_output_columns(
        &mut self,
        sql: &str,
        tables: &[TableDef],
    ) -> Result<Vec<OutputColumn>, Error> {
        self.run_analysis(
            sql,
            CatalogContents::tables_only(tables),
            AnalysisOptions::default(),
            Self::output_columns,
        )
    }

    /// Analyzes a query and returns the tables it reads, each with the columns
    /// actually referenced.
    ///
    /// Walks the resolved tree for every table scan, so the reported columns are
    /// pruned to those the query needs. Non-query statements read no tables and
    /// yield an empty vec. Returns [`Error::GoogleSql`] on a syntax error or
    /// unresolved name.
    pub fn referenced_tables(
        &mut self,
        sql: &str,
        tables: &[TableDef],
    ) -> Result<Vec<TableRef>, Error> {
        let opts = AnalysisOptions {
            prune_columns: true,
            ..AnalysisOptions::default()
        };
        self.run_analysis(
            sql,
            CatalogContents::tables_only(tables),
            opts,
            Self::referenced_tables_of,
        )
    }

    /// Analyzes a statement and returns its resolved AST as a self-contained tree.
    ///
    /// Each [`ResolvedNode`] exposes its kind and children, mirroring the parser's
    /// [`Module::parse_statement`] tree for the analyzer's typed output. Returns
    /// `Ok(None)` for a statement that produces no resolved output, or
    /// [`Error::GoogleSql`] on a syntax error or unresolved name.
    pub fn resolved_tree(
        &mut self,
        sql: &str,
        tables: &[TableDef],
    ) -> Result<Option<ResolvedNode>, Error> {
        let opts = AnalysisOptions {
            record_parse_locations: true,
            ..AnalysisOptions::default()
        };
        self.run_analysis(
            sql,
            CatalogContents::tables_only(tables),
            opts,
            Self::resolved_tree_of,
        )
    }

    /// Analyzes a SQL statement against `catalog`, which may declare both tables
    /// and user-defined functions.
    ///
    /// The [`Catalog`] counterpart of [`Module::analyze_statement_with_catalog`]:
    /// besides tables, any [`FunctionDef`] it carries is registered so calls to
    /// that function resolve. Returns [`Error::GoogleSql`] on a syntax error or
    /// unresolved name.
    pub fn analyze_statement_in(&mut self, sql: &str, catalog: &Catalog) -> Result<(), Error> {
        self.run_analysis(
            sql,
            CatalogContents::of(catalog),
            AnalysisOptions::default(),
            |_, _| Ok(()),
        )
    }

    /// Analyzes a standalone scalar expression against `catalog` and returns its
    /// inferred type.
    ///
    /// The [`Catalog`] counterpart of [`Module::analyze_expression`]: besides
    /// tables, the expression may reference the catalog's constants and
    /// user-defined functions.
    pub fn analyze_expression_in(&mut self, sql: &str, catalog: &Catalog) -> Result<String, Error> {
        self.run_analysis(
            sql,
            CatalogContents::of(catalog),
            AnalysisOptions {
                target: AnalysisTarget::Expression,
                ..AnalysisOptions::default()
            },
            Self::expression_type,
        )
    }

    /// Analyzes every statement in a possibly multi-statement `sql` against
    /// `catalog`, returning how many statements were analyzed.
    ///
    /// The [`Catalog`] counterpart of [`Module::analyze_statements`]: each
    /// statement resolves against the catalog's tables and user-defined functions.
    pub fn analyze_statements_in(&mut self, sql: &str, catalog: &Catalog) -> Result<usize, Error> {
        let analyzed = self.run_multi_analysis(
            sql,
            CatalogContents::of(catalog),
            AnalysisOptions::default(),
            |_, _| Ok(()),
        )?;
        Ok(analyzed.len())
    }

    /// Resolves a type name against `catalog` to its canonical resolved type name.
    ///
    /// The [`Catalog`] counterpart of [`Module::analyze_type`]: `type_name` may
    /// reference the catalog's named types (and other catalog-defined type names),
    /// which resolve to their underlying type.
    pub fn analyze_type_in(&mut self, type_name: &str, catalog: &Catalog) -> Result<String, Error> {
        self.run_analysis(
            type_name,
            CatalogContents::of(catalog),
            AnalysisOptions {
                target: AnalysisTarget::Type,
                ..AnalysisOptions::default()
            },
            Self::resolved_type_name,
        )
    }

    /// Analyzes a query against `catalog` and returns its resolved output schema.
    ///
    /// The [`Catalog`] counterpart of [`Module::analyze_output_columns`], also
    /// resolving calls to the catalog's user-defined functions.
    pub fn analyze_output_columns_in(
        &mut self,
        sql: &str,
        catalog: &Catalog,
    ) -> Result<Vec<OutputColumn>, Error> {
        self.run_analysis(
            sql,
            CatalogContents::of(catalog),
            AnalysisOptions::default(),
            Self::output_columns,
        )
    }

    /// Analyzes a query against `catalog` and returns the tables it reads, each
    /// with the columns actually referenced.
    ///
    /// The [`Catalog`] counterpart of [`Module::referenced_tables`].
    pub fn referenced_tables_in(
        &mut self,
        sql: &str,
        catalog: &Catalog,
    ) -> Result<Vec<TableRef>, Error> {
        let opts = AnalysisOptions {
            prune_columns: true,
            ..AnalysisOptions::default()
        };
        self.run_analysis(
            sql,
            CatalogContents::of(catalog),
            opts,
            Self::referenced_tables_of,
        )
    }

    /// Analyzes a statement against `catalog` and returns its resolved AST as a
    /// self-contained tree.
    ///
    /// The [`Catalog`] counterpart of [`Module::resolved_tree`].
    pub fn resolved_tree_in(
        &mut self,
        sql: &str,
        catalog: &Catalog,
    ) -> Result<Option<ResolvedNode>, Error> {
        let opts = AnalysisOptions {
            record_parse_locations: true,
            ..AnalysisOptions::default()
        };
        self.run_analysis(
            sql,
            CatalogContents::of(catalog),
            opts,
            Self::resolved_tree_of,
        )
    }

    /// Runs the full analysis pipeline, invoking `extract` on the resulting
    /// `AnalyzerOutput` while every analyzer handle is still alive.
    ///
    /// When `prune_columns` is set, table scans expose only the columns the query
    /// references; this matters only for extractors that inspect scan columns
    /// (i.e. lineage), so the schema/success APIs leave it off.
    ///
    /// Every wasm-side handle acquired during the analysis is an RAII [`Handle`]
    /// that enqueues its own free on drop; the enclosing [`with_frees`](Module::with_frees)
    /// releases them all, whether the analysis succeeded or failed. All the
    /// handles stay alive until `extract` returns, so it reads the `AnalyzerOutput`
    /// (and the catalog/type factory that own its nodes and types) intact.
    fn run_analysis<T>(
        &mut self,
        sql: &str,
        catalog: CatalogContents,
        opts: AnalysisOptions,
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // The single-statement pipeline: one `AnalyzeStatement`/`AnalyzeExpression`/
        // `AnalyzeType` call, then `extract` on its output.
        self.run_pipeline(sql, catalog, opts, move |module, sql, call, catalog, tf| {
            module.analyze(sql, call, catalog, tf, extract)
        })
    }

    /// Runs the analysis pipeline over every statement in a possibly
    /// multi-statement `sql`, invoking `extract` on each statement's
    /// `AnalyzerOutput` in turn and collecting the results.
    ///
    /// The catalog and options are built once and shared across all statements, so
    /// the extractor sees each statement resolved against the same catalog. A
    /// statement that fails analysis stops the run and its error is returned.
    fn run_multi_analysis<T>(
        &mut self,
        sql: &str,
        catalog: CatalogContents,
        opts: AnalysisOptions,
        extract: impl FnMut(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        self.run_pipeline(sql, catalog, opts, move |module, sql, call, catalog, tf| {
            module.analyze_each(sql, call, catalog, tf, extract)
        })
    }

    /// Sets up the shared analysis state — `AnalyzerOptions`, language features,
    /// the catalog, and the type factory — then hands control to `terminal`, which
    /// performs the actual analyzer call(s) and reads their output while every
    /// handle is still alive.
    ///
    /// When `prune_columns` is set, table scans expose only the columns the query
    /// references; this matters only for extractors that inspect scan columns
    /// (i.e. lineage), so the schema/success APIs leave it off.
    ///
    /// Every wasm-side handle acquired during the analysis is an RAII [`Handle`]
    /// that enqueues its own free on drop; the enclosing [`with_frees`](Module::with_frees)
    /// releases them all, whether the analysis succeeded or failed. All the
    /// handles stay alive until `terminal` returns, so it reads the `AnalyzerOutput`
    /// (and the catalog/type factory that own its nodes and types) intact.
    fn run_pipeline<R>(
        &mut self,
        sql: &str,
        catalog: CatalogContents,
        opts: AnalysisOptions,
        terminal: impl FnOnce(&mut Self, &str, AnalyzerCall, u64, u64) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.with_frees(move |module| {
            let options = module.acquire_handle(
                SVC_ANALYZER_OPTIONS,
                MID_NEW_ANALYZER_OPTIONS,
                &[],
                SVC_ANALYZER_OPTIONS,
                MID_FREE_ANALYZER_OPTIONS,
            )?;
            // Declared parameters switch on strict parameter mode (undeclared
            // parameters rejected, declared ones type-checked); with none
            // declared, undeclared parameters stay allowed so a bare `@param`
            // resolves with an inferred type.
            module.configure_options(options.ptr(), opts, catalog.parameters.is_empty())?;
            // Enable the maximum language feature set so gated syntax such as the
            // `QUALIFY` clause resolves.
            let language = module.language_options()?;
            module.set_options_language(options.ptr(), language)?;
            let call = AnalyzerCall {
                options: options.ptr(),
                mid: opts.target.mid(),
                frees_output: opts.target.frees_output(),
            };
            module.analyze_with_options(sql, call, catalog, terminal)
        })
    }

    /// Applies analysis options. When `allow_undeclared_parameters` is set, a
    /// statement using `@param` resolves (with the parameter's type inferred from
    /// context) instead of erroring; this leaves parameter-free statements
    /// unaffected. It is cleared when the caller declares typed parameters, which
    /// are mutually exclusive with undeclared ones in GoogleSQL. Enables column
    /// pruning when requested so table scans expose only referenced columns, and
    /// parse-location recording so resolved nodes carry their source byte range.
    fn configure_options(
        &mut self,
        options: u64,
        opts: AnalysisOptions,
        allow_undeclared_parameters: bool,
    ) -> Result<(), Error> {
        self.set_options_bool(
            options,
            MID_SET_ALLOW_UNDECLARED_PARAMETERS,
            allow_undeclared_parameters,
        )?;
        if opts.prune_columns {
            self.set_options_bool(options, MID_SET_PRUNE_UNUSED_COLUMNS, true)?;
        }
        if opts.record_parse_locations {
            self.set_options_int32(
                options,
                MID_SET_PARSE_LOCATION_RECORD_TYPE,
                PARSE_LOCATION_RECORD_FULL_NODE_SCOPE,
            )?;
        }
        Ok(())
    }

    /// Wires a `LanguageOptions` handle into an `AnalyzerOptions` handle.
    fn set_options_language(&mut self, options: u64, language: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, options);
        pb::append_handle(&mut req, 2, language);
        let resp = self.invoke(SVC_ANALYZER_OPTIONS, MID_SET_LANGUAGE, &req)?;
        check_error(&resp)
    }

    /// Sets one boolean flag on an `AnalyzerOptions` handle.
    fn set_options_bool(&mut self, options: u64, mid: i32, value: bool) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, options);
        pb::append_bool(&mut req, 2, value);
        let resp = self.invoke(SVC_ANALYZER_OPTIONS, mid, &req)?;
        check_error(&resp)
    }

    /// Sets one enum/int32 option on an `AnalyzerOptions` handle.
    fn set_options_int32(&mut self, options: u64, mid: i32, value: i32) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, options);
        pb::append_int32(&mut req, 2, value);
        let resp = self.invoke(SVC_ANALYZER_OPTIONS, mid, &req)?;
        check_error(&resp)
    }

    /// Builds the `TypeFactory` handle and runs the analysis against it. The
    /// factory outlives the analysis (it owns the resolved output's types) and is
    /// freed by the top-level [`flush_frees`](Module::flush_frees).
    fn analyze_with_options<R>(
        &mut self,
        sql: &str,
        call: AnalyzerCall,
        catalog: CatalogContents,
        terminal: impl FnOnce(&mut Self, &str, AnalyzerCall, u64, u64) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let type_factory = self.acquire_handle(
            SVC_TYPE_FACTORY,
            MID_NEW_TYPE_FACTORY,
            &[],
            SVC_TYPE_FACTORY,
            MID_FREE_TYPE_FACTORY,
        )?;
        self.analyze_with_catalog(sql, call, type_factory.ptr(), catalog, terminal)
    }

    /// Builds a `SimpleCatalog` handle over `type_factory`, populates it with the
    /// catalog contents, and runs the analysis. The catalog outlives the analysis
    /// (it owns the resolved output's nodes) and is freed by the top-level
    /// [`flush_frees`](Module::flush_frees).
    fn analyze_with_catalog<R>(
        &mut self,
        sql: &str,
        call: AnalyzerCall,
        type_factory: u64,
        contents: CatalogContents,
        terminal: impl FnOnce(&mut Self, &str, AnalyzerCall, u64, u64) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let mut catalog_req = Vec::new();
        pb::append_string(&mut catalog_req, 1, "");
        pb::append_handle(&mut catalog_req, 2, type_factory);
        let catalog = self.acquire_handle(
            SVC_SIMPLE_CATALOG,
            MID_NEW_SIMPLE_CATALOG,
            &catalog_req,
            SVC_SIMPLE_CATALOG,
            MID_FREE_SIMPLE_CATALOG,
        )?;
        self.add_builtin_functions(catalog.ptr())?;
        self.populate_and_analyze(sql, call, catalog.ptr(), type_factory, contents, terminal)
    }

    /// Registers a catalog's declarations and runs the analysis via `terminal`.
    /// Every handle created while populating the catalog stays alive until
    /// `terminal` returns and is freed by the top-level
    /// [`flush_frees`](Module::flush_frees).
    fn populate_and_analyze<R>(
        &mut self,
        sql: &str,
        call: AnalyzerCall,
        catalog: u64,
        type_factory: u64,
        contents: CatalogContents,
        terminal: impl FnOnce(&mut Self, &str, AnalyzerCall, u64, u64) -> Result<R, Error>,
    ) -> Result<R, Error> {
        // Bound (not `_`) so these handles stay alive across the analysis and
        // enqueue their frees only after it returns: dropping them earlier would
        // order their frees ahead of the AnalyzerOutput that references them.
        //
        // The descriptor pool is built first so proto type resolution during
        // catalog population can find its messages; proto descriptors are a
        // top-level setting, like query parameters below.
        let mut handles = Vec::new();
        self.setup_descriptor_pool(contents.proto_files, &mut handles)?;
        handles.extend(self.populate_catalog(catalog, type_factory, contents)?);
        // Query parameters are an analysis-wide `AnalyzerOptions` setting rather
        // than a catalog declaration, so only the top-level contents supply them.
        self.add_query_parameters(call.options, type_factory, contents.parameters)?;
        let result = terminal(self, sql, call, catalog, type_factory);
        drop(handles);
        result
    }

    /// Registers every declaration in `contents` into `catalog`, recursing into
    /// nested sub-catalogs, and returns all wasm-side handles created so the
    /// caller keeps them alive across the analysis.
    ///
    /// A sub-catalog is a plain [`SimpleCatalog`] added under its name via
    /// `AddCatalog2`, which does not take ownership; like the tables and
    /// functions it holds, it is referenced by the resolved output and so must
    /// outlive the analysis. Sub-catalogs intentionally do not receive the
    /// builtin functions and types (added only to the root), so they act as pure
    /// namespaces. A failure part way through drops the handles built so far,
    /// enqueueing their frees.
    fn populate_catalog(
        &mut self,
        catalog: u64,
        type_factory: u64,
        contents: CatalogContents,
    ) -> Result<Vec<Handle>, Error> {
        let mut handles = self.add_tables(catalog, type_factory, contents.tables)?;
        handles.extend(self.add_functions(catalog, type_factory, contents.functions)?);
        handles.extend(self.add_constants(catalog, contents.constants)?);
        handles.extend(self.add_table_functions(
            catalog,
            type_factory,
            contents.table_functions,
        )?);
        self.add_procedures(catalog, type_factory, contents.procedures, &mut handles)?;
        self.add_connections(catalog, contents.connections, &mut handles)?;
        self.add_types(catalog, type_factory, contents.types)?;
        self.add_enums(catalog, type_factory, contents.enums, &mut handles)?;
        self.add_property_graphs(
            catalog,
            type_factory,
            contents.property_graphs,
            &mut handles,
        )?;
        for sub in contents.catalogs {
            let child = self.new_simple_catalog(&sub.name, type_factory)?;
            let child_handles = self.populate_catalog(
                child.ptr(),
                type_factory,
                CatalogContents::of(&sub.catalog),
            )?;
            self.register_catalog(catalog, &sub.name, child.ptr())?;
            handles.extend(child_handles);
            handles.push(child);
        }
        Ok(handles)
    }

    /// Registers each table into `catalog`, returning the created `SimpleTable`
    /// handles so the caller keeps them alive across the analysis. A failure part
    /// way through drops the handles built so far, enqueueing their frees.
    fn add_tables(
        &mut self,
        catalog: u64,
        type_factory: u64,
        tables: &[TableDef],
    ) -> Result<Vec<Handle>, Error> {
        let mut handles = Vec::with_capacity(tables.len());
        for table in tables {
            handles.push(self.add_table(catalog, type_factory, table)?);
        }
        Ok(handles)
    }

    /// Creates a `SimpleTable`, adds its columns, and registers it under its
    /// name in `catalog`, returning the table handle for the caller to keep alive
    /// across the analysis. On failure the partially built table's handle drops,
    /// enqueueing its free.
    fn add_table(
        &mut self,
        catalog: u64,
        type_factory: u64,
        table: &TableDef,
    ) -> Result<Handle, Error> {
        let mut table_req = Vec::new();
        pb::append_string(&mut table_req, 1, &table.name);
        pb::append_uint64(&mut table_req, 2, 0); // serialization id (unused)
        let handle = self.acquire_handle(
            SVC_SIMPLE_TABLE,
            MID_NEW_SIMPLE_TABLE,
            &table_req,
            SVC_SIMPLE_TABLE,
            MID_FREE_SIMPLE_TABLE,
        )?;

        self.add_columns(handle.ptr(), type_factory, &table.name, &table.columns)?;
        self.register_table(catalog, &table.name, handle.ptr())?;
        Ok(handle)
    }

    /// Adds each column to `table`, transferring ownership so freeing the table
    /// frees the columns. A column that cannot be attached is freed before the
    /// error is returned.
    fn add_columns(
        &mut self,
        table: u64,
        type_factory: u64,
        table_name: &str,
        columns: &[ColumnDef],
    ) -> Result<(), Error> {
        for column in columns {
            let type_handle = self.build_column_type(type_factory, &column.ty)?;

            let mut column_req = Vec::new();
            pb::append_string(&mut column_req, 1, table_name);
            pb::append_string(&mut column_req, 2, &column.name);
            pb::append_handle(&mut column_req, 3, type_handle);
            pb::append_bool(&mut column_req, 4, false); // is_pseudo_column
            pb::append_bool(&mut column_req, 5, true); // is_writable_column
            let column_handle =
                self.new_handle(SVC_SIMPLE_COLUMN, MID_NEW_SIMPLE_COLUMN, &column_req)?;

            let mut add_req = Vec::new();
            pb::append_handle(&mut add_req, 1, table);
            pb::append_handle(&mut add_req, 2, column_handle);
            pb::append_bool(&mut add_req, 3, true); // the table takes ownership
            match self
                .invoke(SVC_SIMPLE_TABLE, MID_ADD_COLUMN_OWNED, &add_req)
                .and_then(|resp| check_error(&resp))
            {
                Ok(()) => {}
                Err(e) => {
                    // Best-effort cleanup of the orphaned column while already
                    // unwinding `e`; a second failure here cannot be surfaced
                    // without masking the original error, so it is discarded.
                    #[allow(
                        clippy::let_underscore_must_use,
                        reason = "best-effort free on an error path; original error takes precedence"
                    )]
                    let _ = self.invoke(
                        SVC_SIMPLE_COLUMN,
                        MID_FREE_SIMPLE_COLUMN,
                        &pb::handle_arg(column_handle),
                    );
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Builds a GoogleSQL type handle for `ty` via `type_factory`, recursing into
    /// element types. The returned type is owned by the factory (reclaimed when
    /// the factory is freed), so it is not registered for an individual free —
    /// matching how the scalar getters' types are handled.
    fn build_column_type(&mut self, type_factory: u64, ty: &ColumnType) -> Result<u64, Error> {
        match ty {
            ColumnType::Array(element) => {
                let element_type = self.build_column_type(type_factory, element)?;
                self.make_array_type(type_factory, element_type)
            }
            ColumnType::Struct(fields) => self.make_struct_type(type_factory, fields),
            ColumnType::Range(element) => {
                let element_type = self.build_column_type(type_factory, element)?;
                self.make_range_type(type_factory, element_type)
            }
            ColumnType::Map(key, value) => {
                let key_type = self.build_column_type(type_factory, key)?;
                let value_type = self.build_column_type(type_factory, value)?;
                self.make_map_type(type_factory, key_type, value_type)
            }
            ColumnType::Proto(name) => {
                let pool = self.descriptor_pool.ok_or_else(|| {
                    Error::Protocol(format!(
                        "proto type {name} requires the catalog to declare proto_files"
                    ))
                })?;
                let message = self.find_message_type(pool, name)?;
                self.make_proto_type(type_factory, message)
            }
            scalar => {
                let mid = scalar.scalar_type_factory_mid().ok_or_else(|| {
                    Error::Protocol("column type has no type factory getter".into())
                })?;
                self.new_handle(SVC_TYPE_FACTORY, mid, &pb::handle_arg(type_factory))
            }
        }
    }

    /// Wraps `element` in an `ARRAY<...>` type via `TypeFactory::MakeArrayType`.
    /// GoogleSQL rejects arrays of arrays, surfacing as [`Error::GoogleSql`].
    fn make_array_type(&mut self, type_factory: u64, element: u64) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_factory);
        pb::append_handle(&mut req, 2, element);
        let resp = self.invoke(SVC_TYPE_FACTORY, MID_MAKE_ARRAY_TYPE, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 2);
        if ptr == 0 {
            return Err(Error::Protocol("MakeArrayType returned null".into()));
        }
        Ok(ptr)
    }

    /// Wraps `element` in a `RANGE<...>` type via `TypeFactory::MakeRangeType`.
    /// GoogleSQL allows only `DATE`/`DATETIME`/`TIMESTAMP` elements; any other
    /// surfaces as [`Error::GoogleSql`].
    fn make_range_type(&mut self, type_factory: u64, element: u64) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_factory);
        pb::append_handle(&mut req, 2, element);
        let resp = self.invoke(SVC_TYPE_FACTORY, MID_MAKE_RANGE_TYPE, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 2);
        if ptr == 0 {
            return Err(Error::Protocol("MakeRangeType returned null".into()));
        }
        Ok(ptr)
    }

    /// Builds a `MAP<K, V>` type from `key` and `value` via
    /// `TypeFactory::MakeMapType`. GoogleSQL requires the key type to support
    /// grouping and gates the type behind a language feature; an unsupported key
    /// or a disabled feature surfaces as [`Error::GoogleSql`].
    fn make_map_type(&mut self, type_factory: u64, key: u64, value: u64) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_factory);
        pb::append_handle(&mut req, 2, key);
        pb::append_handle(&mut req, 3, value);
        let resp = self.invoke(SVC_TYPE_FACTORY, MID_MAKE_MAP_TYPE, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 1);
        if ptr == 0 {
            return Err(Error::Protocol("MakeMapType returned null".into()));
        }
        Ok(ptr)
    }

    /// Assembles a `STRUCT<...>` type from `fields` via `TypeFactory::MakeStructType`.
    ///
    /// Each field's type is built first (recursing through the same builder), then
    /// carried as a `{name, type}` submessage repeated at field 2 of the request.
    fn make_struct_type(
        &mut self,
        type_factory: u64,
        fields: &[StructField],
    ) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_factory);
        for field in fields {
            let field_type = self.build_column_type(type_factory, &field.ty)?;
            let mut field_msg = Vec::new();
            pb::append_string(&mut field_msg, 1, &field.name);
            pb::append_handle(&mut field_msg, 2, field_type);
            pb::append_submessage(&mut req, 2, &field_msg);
        }
        let resp = self.invoke(SVC_TYPE_FACTORY, MID_MAKE_STRUCT_TYPE, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 2);
        if ptr == 0 {
            return Err(Error::Protocol("MakeStructType returned null".into()));
        }
        Ok(ptr)
    }

    /// Registers `table` under `name` in `catalog` (non-owning).
    fn register_table(&mut self, catalog: u64, name: &str, table: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, table);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_TABLE_NAMED, &req)?;
        check_error(&resp)
    }

    /// Builds a named, empty `SimpleCatalog` over `type_factory`, returning its
    /// handle for the caller to populate and keep alive across the analysis.
    fn new_simple_catalog(&mut self, name: &str, type_factory: u64) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name);
        pb::append_handle(&mut req, 2, type_factory);
        self.acquire_handle(
            SVC_SIMPLE_CATALOG,
            MID_NEW_SIMPLE_CATALOG,
            &req,
            SVC_SIMPLE_CATALOG,
            MID_FREE_SIMPLE_CATALOG,
        )
    }

    /// Registers `child` as a nested catalog under `name` in `parent`
    /// (non-owning: `parent` references `child` without freeing it).
    fn register_catalog(&mut self, parent: u64, name: &str, child: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, parent);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, child);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_CATALOG_NAMED, &req)?;
        check_error(&resp)
    }

    /// Registers each function into `catalog`, returning every wasm-side handle
    /// created along the way so the caller keeps them alive across the analysis.
    ///
    /// A `Function` references its `FunctionSignature`, which references its
    /// `FunctionArgumentType`s, all by raw pointer; the catalog references the
    /// `Function` without owning it. So none may be freed until the analysis (and
    /// the `AnalyzerOutput` that may reference the function) is done — the caller
    /// holds the returned handles until then.
    fn add_functions(
        &mut self,
        catalog: u64,
        type_factory: u64,
        functions: &[FunctionDef],
    ) -> Result<Vec<Handle>, Error> {
        let mut handles = Vec::with_capacity(functions.len());
        for function in functions {
            self.add_function(catalog, type_factory, function, &mut handles)?;
        }
        Ok(handles)
    }

    /// Builds a `Function` from `function` and registers it into `catalog`.
    /// Every acquired handle is pushed onto `handles` so it outlives the analysis.
    fn add_function(
        &mut self,
        catalog: u64,
        type_factory: u64,
        function: &FunctionDef,
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        let result_type = self.build_column_type(type_factory, &function.return_type)?;
        let result_arg = self.new_function_argument_type(result_type)?;

        let argument_ptrs =
            self.build_argument_types(type_factory, &function.arguments, handles)?;

        let signature = self.new_function_signature(result_arg.ptr(), &argument_ptrs)?;
        let handle = self.new_function(&function.name, function.kind.mode(), signature.ptr())?;
        self.register_function(catalog, handle.ptr())?;

        handles.push(result_arg);
        handles.push(signature);
        handles.push(handle);
        Ok(())
    }

    /// Wraps a type handle in a required-once `FunctionArgumentType`.
    fn new_function_argument_type(&mut self, type_handle: u64) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_handle);
        pb::append_int32(&mut req, 2, ARGUMENT_REQUIRED_OCCURRENCES);
        self.acquire_handle(
            SVC_FUNCTION_ARGUMENT_TYPE,
            MID_NEW_FUNCTION_ARGUMENT_TYPE,
            &req,
            SVC_FUNCTION_ARGUMENT_TYPE,
            MID_FREE_FUNCTION_ARGUMENT_TYPE,
        )
    }

    /// Wraps each type in `arguments` in a `FunctionArgumentType`, returning the
    /// resulting handles' raw pointers in order for a `FunctionSignature`. Every
    /// created handle is pushed onto `handles` so it outlives the analysis: the
    /// signature references these argument types by raw pointer and must not see
    /// them freed.
    fn build_argument_types(
        &mut self,
        type_factory: u64,
        arguments: &[ColumnType],
        handles: &mut Vec<Handle>,
    ) -> Result<Vec<u64>, Error> {
        let mut argument_ptrs = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let argument_type = self.build_column_type(type_factory, argument)?;
            let argument_arg = self.new_function_argument_type(argument_type)?;
            argument_ptrs.push(argument_arg.ptr());
            handles.push(argument_arg);
        }
        Ok(argument_ptrs)
    }

    /// Builds a `FunctionSignature` from a result and argument `FunctionArgumentType`s.
    fn new_function_signature(
        &mut self,
        result_type: u64,
        argument_types: &[u64],
    ) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, result_type);
        for &argument in argument_types {
            pb::append_handle(&mut req, 2, argument);
        }
        // Field 3 (context id) is left at its proto default of 0.
        self.acquire_handle(
            SVC_FUNCTION_SIGNATURE,
            MID_NEW_FUNCTION_SIGNATURE,
            &req,
            SVC_FUNCTION_SIGNATURE,
            MID_FREE_FUNCTION_SIGNATURE,
        )
    }

    /// Builds a `Function` named `name` with the given `mode`, carrying a single
    /// `signature`.
    fn new_function(&mut self, name: &str, mode: i32, signature: u64) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name);
        pb::append_string(&mut req, 2, USER_FUNCTION_GROUP);
        pb::append_int32(&mut req, 3, mode);
        pb::append_handle(&mut req, 4, signature);
        self.acquire_handle(
            SVC_FUNCTION,
            MID_NEW_FUNCTION,
            &req,
            SVC_FUNCTION,
            MID_FREE_FUNCTION,
        )
    }

    /// Registers `function` into `catalog` (non-owning; its name comes from the
    /// `Function` itself).
    fn register_function(&mut self, catalog: u64, function: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_handle(&mut req, 2, function);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_FUNCTION, &req)?;
        check_error(&resp)
    }

    /// Registers each procedure into `catalog`, pushing every wasm-side handle
    /// created along the way onto `handles` so the caller keeps them alive across
    /// the analysis (a `Procedure` references its `FunctionSignature` and
    /// argument types by raw pointer, and the catalog references it without
    /// owning it).
    fn add_procedures(
        &mut self,
        catalog: u64,
        type_factory: u64,
        procedures: &[ProcedureDef],
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        for procedure in procedures {
            self.add_procedure(catalog, type_factory, procedure, handles)?;
        }
        Ok(())
    }

    /// Builds a `Procedure` from `procedure` and registers it under its name in
    /// `catalog`. A procedure carries a `FunctionSignature` like a function; it
    /// has no expression result, so the signature's result type is an
    /// unused `INT64` placeholder and only the argument types are meaningful.
    fn add_procedure(
        &mut self,
        catalog: u64,
        type_factory: u64,
        procedure: &ProcedureDef,
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        let result_type = self.build_column_type(type_factory, &ColumnType::Int64)?;
        let result_arg = self.new_function_argument_type(result_type)?;

        let argument_ptrs =
            self.build_argument_types(type_factory, &procedure.arguments, handles)?;

        let signature = self.new_function_signature(result_arg.ptr(), &argument_ptrs)?;
        let handle = self.new_procedure(&procedure.name, signature.ptr())?;
        self.register_procedure(catalog, &procedure.name, handle.ptr())?;

        handles.push(result_arg);
        handles.push(signature);
        handles.push(handle);
        Ok(())
    }

    /// Builds a `Procedure` named `name` carrying a single `signature`. The name
    /// is passed as a one-element name path.
    fn new_procedure(&mut self, name: &str, signature: u64) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name);
        pb::append_handle(&mut req, 2, signature);
        self.acquire_handle(
            SVC_PROCEDURE,
            MID_NEW_PROCEDURE,
            &req,
            SVC_PROCEDURE,
            MID_FREE_PROCEDURE,
        )
    }

    /// Registers `procedure` under `name` in `catalog` (non-owning).
    fn register_procedure(
        &mut self,
        catalog: u64,
        name: &str,
        procedure: u64,
    ) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, procedure);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_PROCEDURE_NAMED, &req)?;
        check_error(&resp)
    }

    /// Registers each external connection into `catalog`, pushing the created
    /// `SimpleConnection` handles onto `handles` so they outlive the analysis
    /// (the catalog references each connection without owning it).
    fn add_connections(
        &mut self,
        catalog: u64,
        connections: &[String],
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        for name in connections {
            let connection = self.new_simple_connection(name)?;
            self.register_connection(catalog, name, connection.ptr())?;
            handles.push(connection);
        }
        Ok(())
    }

    /// Builds a named `SimpleConnection`, returning its handle for the caller to
    /// keep alive across the analysis.
    fn new_simple_connection(&mut self, name: &str) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name);
        self.acquire_handle(
            SVC_SIMPLE_CONNECTION,
            MID_NEW_SIMPLE_CONNECTION,
            &req,
            SVC_SIMPLE_CONNECTION,
            MID_FREE_SIMPLE_CONNECTION,
        )
    }

    /// Registers `connection` under `name` in `catalog` (non-owning).
    fn register_connection(
        &mut self,
        catalog: u64,
        name: &str,
        connection: u64,
    ) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, connection);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_CONNECTION_NAMED, &req)?;
        check_error(&resp)
    }

    /// Registers each named type into `catalog`. The type itself is owned by the
    /// `type_factory` (which outlives the analysis), so no handle is retained —
    /// the catalog only stores the name-to-type alias.
    fn add_types(
        &mut self,
        catalog: u64,
        type_factory: u64,
        types: &[NamedType],
    ) -> Result<(), Error> {
        for named in types {
            let type_handle = self.build_column_type(type_factory, &named.ty)?;
            self.register_type(catalog, &named.name, type_handle)?;
        }
        Ok(())
    }

    /// Registers `type_handle` under `name` in `catalog` (non-owning; the type is
    /// owned by the `TypeFactory`).
    fn register_type(&mut self, catalog: u64, name: &str, type_handle: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, type_handle);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_TYPE_NAMED, &req)?;
        check_error(&resp)
    }

    /// Registers each enum type into `catalog`. Each enum is materialized as a
    /// protobuf `EnumDescriptor`: a `FileDescriptorProto` carrying the enum is
    /// wire-encoded, parsed into a shared `DescriptorPool`, built, and the
    /// resulting `EnumType` is registered under the enum's name.
    ///
    /// The pool is pushed onto `handles` so it outlives the analysis: the
    /// registered `EnumType` — and any resolved output referencing it — points
    /// into the pool's descriptors, so freeing the pool early would leave those
    /// references dangling. The source `FileDescriptorProto`s are retained the
    /// same way; `BuildFile` copies them into the pool, so this is conservative
    /// rather than required.
    fn add_enums(
        &mut self,
        catalog: u64,
        type_factory: u64,
        enums: &[EnumDef],
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        if enums.is_empty() {
            return Ok(());
        }
        let pool = self.acquire_handle(
            SVC_DESCRIPTOR_POOL,
            MID_NEW_DESCRIPTOR_POOL,
            &[],
            SVC_DESCRIPTOR_POOL,
            MID_FREE_DESCRIPTOR_POOL,
        )?;
        for (index, def) in enums.iter().enumerate() {
            let fdp = self.build_enum_file(pool.ptr(), def, index)?;
            let enum_desc = self.find_enum_type(pool.ptr(), &def.name)?;
            let enum_type = self.make_enum_type(type_factory, enum_desc)?;
            self.register_type(catalog, &def.name, enum_type)?;
            handles.push(fdp);
        }
        handles.push(pool);
        Ok(())
    }

    /// Wire-encodes `def` as a single-enum `FileDescriptorProto` and builds it
    /// into `pool` so the enum becomes findable by name. Returns the source
    /// `FileDescriptorProto` handle for the caller to keep alive.
    fn build_enum_file(&mut self, pool: u64, def: &EnumDef, index: usize) -> Result<Handle, Error> {
        let bytes = encode_enum_fdp(def, index);
        self.build_file_into_pool(pool, &bytes)
    }

    /// Builds `proto_files` (each a serialized `FileDescriptorProto`) into a
    /// fresh `DescriptorPool` and records it as the pool in effect for proto type
    /// resolution, or clears that pool when there are no descriptors.
    ///
    /// The pool and source protos are pushed onto `handles` so they outlive the
    /// analysis: a resolved [`ColumnType::Proto`] points into the pool's
    /// descriptors, so freeing it early would leave those references dangling.
    fn setup_descriptor_pool(
        &mut self,
        proto_files: &[Vec<u8>],
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        self.descriptor_pool = None;
        if proto_files.is_empty() {
            return Ok(());
        }
        let pool = self.acquire_handle(
            SVC_DESCRIPTOR_POOL,
            MID_NEW_DESCRIPTOR_POOL,
            &[],
            SVC_DESCRIPTOR_POOL,
            MID_FREE_DESCRIPTOR_POOL,
        )?;
        for bytes in proto_files {
            let fdp = self.build_file_into_pool(pool.ptr(), bytes)?;
            handles.push(fdp);
        }
        self.descriptor_pool = Some(pool.ptr());
        handles.push(pool);
        Ok(())
    }

    /// Creates a `FileDescriptorProto`, parses the serialized `bytes` into it via
    /// the `MessageLite` base (rather than field-by-field setter calls), and
    /// builds it into `pool` so its descriptors become resolvable. Returns the
    /// proto handle for the caller to keep alive.
    fn build_file_into_pool(&mut self, pool: u64, bytes: &[u8]) -> Result<Handle, Error> {
        let fdp = self.acquire_handle(
            SVC_FILE_DESCRIPTOR_PROTO,
            MID_NEW_FILE_DESCRIPTOR_PROTO,
            &[],
            SVC_FILE_DESCRIPTOR_PROTO,
            MID_FREE_FILE_DESCRIPTOR_PROTO,
        )?;
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, fdp.ptr());
        pb::append_submessage(&mut req, 2, bytes);
        let resp = self.invoke(SVC_MESSAGE_LITE, MID_PARSE_FROM_STRING, &req)?;
        check_error(&resp)?;
        if !pb::read_bool_at_field(&resp, 1) {
            return Err(Error::Protocol(
                "ParseFromString rejected the descriptor".into(),
            ));
        }
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, pool);
        pb::append_handle(&mut req, 2, fdp.ptr());
        let resp = self.invoke(SVC_DESCRIPTOR_POOL, MID_BUILD_FILE, &req)?;
        check_error(&resp)?;
        if pb::read_handle_at_field(&resp, 1) == 0 {
            return Err(Error::Protocol("BuildFile returned null".into()));
        }
        Ok(fdp)
    }

    /// Looks up an enum `EnumDescriptor` by its full name in `pool`.
    fn find_enum_type(&mut self, pool: u64, name: &str) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, pool);
        pb::append_string(&mut req, 2, name);
        let resp = self.invoke(SVC_DESCRIPTOR_POOL, MID_FIND_ENUM_TYPE_BY_NAME, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 1);
        if ptr == 0 {
            return Err(Error::Protocol(format!("enum type {name} not found")));
        }
        Ok(ptr)
    }

    /// Builds the `EnumType` for `enum_desc`. The resulting type is owned by the
    /// `type_factory`, so the caller retains no handle for it. The handle is read
    /// from response field 2 (field 1 carries the abstract type-node variant).
    fn make_enum_type(&mut self, type_factory: u64, enum_desc: u64) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_factory);
        pb::append_handle(&mut req, 2, enum_desc);
        let resp = self.invoke(SVC_TYPE_FACTORY, MID_MAKE_ENUM_TYPE, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 2);
        if ptr == 0 {
            return Err(Error::Protocol("MakeEnumType returned null".into()));
        }
        Ok(ptr)
    }

    /// Looks up a protobuf message `Descriptor` by its full name in `pool`.
    fn find_message_type(&mut self, pool: u64, name: &str) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, pool);
        pb::append_string(&mut req, 2, name);
        let resp = self.invoke(SVC_DESCRIPTOR_POOL, MID_FIND_MESSAGE_TYPE_BY_NAME, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 1);
        if ptr == 0 {
            return Err(Error::Protocol(format!("proto type {name} not found")));
        }
        Ok(ptr)
    }

    /// Builds the `ProtoType` for `message`. The resulting type is owned by the
    /// `type_factory`, so the caller retains no handle for it. The handle is read
    /// from response field 2 (field 1 carries the abstract type-node variant).
    fn make_proto_type(&mut self, type_factory: u64, message: u64) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, type_factory);
        pb::append_handle(&mut req, 2, message);
        let resp = self.invoke(SVC_TYPE_FACTORY, MID_MAKE_PROTO_TYPE, &req)?;
        check_error(&resp)?;
        let ptr = pb::read_handle_at_field(&resp, 2);
        if ptr == 0 {
            return Err(Error::Protocol("MakeProtoType returned null".into()));
        }
        Ok(ptr)
    }

    /// Registers each property graph into `catalog`. Each graph's handle (and
    /// the input `SimpleTable` handles backing its node tables) is pushed onto
    /// `handles` so it outlives the analysis: the resolved output points into
    /// them.
    fn add_property_graphs(
        &mut self,
        catalog: u64,
        type_factory: u64,
        graphs: &[PropertyGraphDef],
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        for graph in graphs {
            self.add_property_graph(catalog, type_factory, graph, handles)?;
        }
        Ok(())
    }

    /// Assembles a `SimplePropertyGraph` from `graph` — its node and edge tables,
    /// the labels they expose, and the property declarations/definitions backing
    /// those labels — and registers it into `catalog` under its name.
    ///
    /// Each `Add*` on the graph transfers ownership, so the graph owns its
    /// labels, node/edge tables, and property declarations (and an element table
    /// owns its property definitions); freeing the graph reclaims them all. The
    /// catalog only references the graph (`AddPropertyGraph2` is non-owning), so
    /// the graph handle — and each element table's input `SimpleTable`, which the
    /// table references rather than owns — are kept alive on `handles` until the
    /// analysis completes.
    ///
    /// Node tables are built first so their handles are available for edge
    /// tables' source/destination references (an edge references a node table
    /// non-owningly, and the reference stays valid after `AddNodeTable` hands the
    /// node table to the graph).
    fn add_property_graph(
        &mut self,
        catalog: u64,
        type_factory: u64,
        graph: &PropertyGraphDef,
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        let graph_handle = self.new_property_graph(&graph.name)?;
        let mut node_ptrs = Vec::with_capacity(graph.node_tables.len());
        for node in &graph.node_tables {
            let node_ptr = self.add_graph_node_table(
                graph_handle.ptr(),
                type_factory,
                &graph.name,
                node,
                handles,
            )?;
            node_ptrs.push((node.name.as_str(), node_ptr));
        }
        for edge in &graph.edge_tables {
            self.add_graph_edge_table(
                graph_handle.ptr(),
                type_factory,
                &graph.name,
                edge,
                &node_ptrs,
                handles,
            )?;
        }
        self.register_property_graph(catalog, &graph.name, graph_handle.ptr())?;
        handles.push(graph_handle);
        Ok(())
    }

    /// Builds an empty `SimplePropertyGraph` whose name path is the single
    /// element `name`, for the caller to populate and keep alive.
    fn new_property_graph(&mut self, name: &str) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name); // single-element name path
        self.acquire_handle(
            SVC_SIMPLE_PROPERTY_GRAPH,
            MID_NEW_SIMPLE_PROPERTY_GRAPH,
            &req,
            SVC_SIMPLE_PROPERTY_GRAPH,
            MID_FREE_SIMPLE_PROPERTY_GRAPH,
        )
    }

    /// Builds one node table into `graph`: its input `SimpleTable`, the labels
    /// it exposes (with their property declarations and definitions), and the
    /// node table itself. The input table handle is pushed onto `handles` (the
    /// node table references it non-owningly, so it must outlive the analysis);
    /// every other object is transferred into and owned by `graph`. Returns the
    /// node table handle so an edge table can reference it.
    fn add_graph_node_table(
        &mut self,
        graph: u64,
        type_factory: u64,
        graph_name: &str,
        node: &GraphNodeTableDef,
        handles: &mut Vec<Handle>,
    ) -> Result<u64, Error> {
        let input = self.add_input_table(type_factory, &node.name, &node.columns)?;
        let (label_ptrs, definition_ptrs) =
            self.build_graph_labels(graph, type_factory, graph_name, &node.labels)?;
        let node_ptr = self.new_graph_node_table(
            &node.name,
            graph_name,
            input.ptr(),
            &node.key_columns,
            &label_ptrs,
            &definition_ptrs,
        )?;
        self.register_graph_node_table(graph, node_ptr)?;
        handles.push(input);
        Ok(node_ptr)
    }

    /// Builds one edge table into `graph`, mirroring
    /// [`add_graph_node_table`](Self::add_graph_node_table) but additionally
    /// pinning each edge to its source and destination node tables via
    /// references looked up in `node_ptrs` (name → node table handle).
    fn add_graph_edge_table(
        &mut self,
        graph: u64,
        type_factory: u64,
        graph_name: &str,
        edge: &GraphEdgeTableDef,
        node_ptrs: &[(&str, u64)],
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        let input = self.add_input_table(type_factory, &edge.name, &edge.columns)?;
        let (label_ptrs, definition_ptrs) =
            self.build_graph_labels(graph, type_factory, graph_name, &edge.labels)?;
        let source = self.new_graph_node_reference(&edge.source, node_ptrs)?;
        let destination = self.new_graph_node_reference(&edge.destination, node_ptrs)?;
        let edge_ptr = self.new_graph_edge_table(
            &edge.name,
            graph_name,
            input.ptr(),
            &edge.key_columns,
            &label_ptrs,
            &definition_ptrs,
            source,
            destination,
        )?;
        self.register_graph_edge_table(graph, edge_ptr)?;
        handles.push(input);
        Ok(())
    }

    /// Builds the property declarations, definitions, and labels for a graph
    /// element table (node or edge). Each declaration and label is added to (and
    /// owned by) `graph`; the returned label handles are referenced by the
    /// element table and the returned definition handles are transferred into it.
    fn build_graph_labels(
        &mut self,
        graph: u64,
        type_factory: u64,
        graph_name: &str,
        labels: &[GraphLabelDef],
    ) -> Result<(Vec<u64>, Vec<u64>), Error> {
        let mut label_ptrs = Vec::with_capacity(labels.len());
        let mut definition_ptrs =
            Vec::with_capacity(labels.iter().map(|label| label.properties.len()).sum());
        for label in labels {
            let mut declaration_ptrs = Vec::with_capacity(label.properties.len());
            for property in &label.properties {
                let property_type = self.build_column_type(type_factory, &property.ty)?;
                let declaration =
                    self.new_graph_property_declaration(&property.name, graph_name, property_type)?;
                self.register_property_declaration(graph, declaration)?;
                let definition =
                    self.new_graph_property_definition(declaration, &property.value_sql)?;
                declaration_ptrs.push(declaration);
                definition_ptrs.push(definition);
            }
            let label_ptr =
                self.new_graph_element_label(&label.name, graph_name, &declaration_ptrs)?;
            self.register_graph_label(graph, label_ptr)?;
            label_ptrs.push(label_ptr);
        }
        Ok((label_ptrs, definition_ptrs))
    }

    /// Builds a `SimpleGraphNodeTableReference` pinning an edge endpoint to the
    /// node table named by `reference`, matching the edge's columns against the
    /// node's. The returned raw handle is transferred into an edge table.
    fn new_graph_node_reference(
        &mut self,
        reference: &GraphNodeReferenceDef,
        node_ptrs: &[(&str, u64)],
    ) -> Result<u64, Error> {
        let node_table = node_ptrs
            .iter()
            .find(|(name, _)| *name == reference.node_table)
            .map(|(_, ptr)| *ptr)
            .ok_or_else(|| {
                Error::Protocol(format!(
                    "edge references unknown node table {}",
                    reference.node_table
                ))
            })?;
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, node_table);
        append_column_indices(&mut req, 2, &reference.edge_columns)?;
        append_column_indices(&mut req, 3, &reference.node_columns)?;
        self.new_handle(
            SVC_GRAPH_NODE_TABLE_REFERENCE,
            MID_NEW_GRAPH_NODE_TABLE_REFERENCE,
            &req,
        )
    }

    /// Builds a standalone `SimpleTable` named `name` with `columns` to back a
    /// node table's input schema. Unlike [`add_table`](Self::add_table) it is not
    /// registered in the catalog — it exists only as the node table's data
    /// source — so the caller keeps its handle alive across the analysis.
    fn add_input_table(
        &mut self,
        type_factory: u64,
        name: &str,
        columns: &[ColumnDef],
    ) -> Result<Handle, Error> {
        let mut table_req = Vec::new();
        pb::append_string(&mut table_req, 1, name);
        pb::append_uint64(&mut table_req, 2, 0); // serialization id (unused)
        let handle = self.acquire_handle(
            SVC_SIMPLE_TABLE,
            MID_NEW_SIMPLE_TABLE,
            &table_req,
            SVC_SIMPLE_TABLE,
            MID_FREE_SIMPLE_TABLE,
        )?;
        self.add_columns(handle.ptr(), type_factory, name, columns)?;
        Ok(handle)
    }

    /// Builds a `SimpleGraphPropertyDeclaration` named `property` of type
    /// `property_type`; the returned raw handle is transferred into the graph by
    /// [`register_property_declaration`](Self::register_property_declaration).
    fn new_graph_property_declaration(
        &mut self,
        property: &str,
        graph_name: &str,
        property_type: u64,
    ) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, property);
        pb::append_string(&mut req, 2, graph_name); // single-element graph name path
        pb::append_handle(&mut req, 3, property_type);
        self.new_handle(
            SVC_GRAPH_PROPERTY_DECLARATION,
            MID_NEW_GRAPH_PROPERTY_DECLARATION,
            &req,
        )
    }

    /// Adds `declaration` to `graph` (`AddPropertyDeclaration`, transferring
    /// ownership).
    fn register_property_declaration(&mut self, graph: u64, declaration: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, graph);
        pb::append_handle(&mut req, 2, declaration);
        let resp = self.invoke(
            SVC_SIMPLE_PROPERTY_GRAPH,
            MID_ADD_GRAPH_PROPERTY_DECLARATION,
            &req,
        )?;
        check_error(&resp)
    }

    /// Builds a `SimpleGraphPropertyDefinition` binding `declaration` to the SQL
    /// expression `value_sql`, evaluated over the element (node or edge) table's
    /// input columns. The returned raw handle is transferred into an element
    /// table's property definitions.
    fn new_graph_property_definition(
        &mut self,
        declaration: u64,
        value_sql: &str,
    ) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, declaration);
        pb::append_string(&mut req, 2, value_sql);
        self.new_handle(
            SVC_GRAPH_PROPERTY_DEFINITION,
            MID_NEW_GRAPH_PROPERTY_DEFINITION,
            &req,
        )
    }

    /// Builds a `SimpleGraphElementLabel` named `label` exposing the properties
    /// declared by `declarations`; the returned raw handle is transferred into
    /// the graph by [`register_graph_label`](Self::register_graph_label).
    fn new_graph_element_label(
        &mut self,
        label: &str,
        graph_name: &str,
        declarations: &[u64],
    ) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, label);
        pb::append_string(&mut req, 2, graph_name); // single-element graph name path
        for &declaration in declarations {
            pb::append_handle(&mut req, 3, declaration);
        }
        self.new_handle(SVC_GRAPH_ELEMENT_LABEL, MID_NEW_GRAPH_ELEMENT_LABEL, &req)
    }

    /// Adds `label` to `graph` (`AddLabel`, transferring ownership).
    fn register_graph_label(&mut self, graph: u64, label: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, graph);
        pb::append_handle(&mut req, 2, label);
        let resp = self.invoke(SVC_SIMPLE_PROPERTY_GRAPH, MID_ADD_GRAPH_LABEL, &req)?;
        check_error(&resp)
    }

    /// Builds a `SimpleGraphNodeTable` over the `input` table, keyed by
    /// `key_columns` (indices into the input schema), exposing `labels` and
    /// defining `definitions`. The returned raw handle is transferred into the
    /// graph by [`register_graph_node_table`](Self::register_graph_node_table).
    fn new_graph_node_table(
        &mut self,
        name: &str,
        graph_name: &str,
        input: u64,
        key_columns: &[u32],
        labels: &[u64],
        definitions: &[u64],
    ) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name);
        pb::append_string(&mut req, 2, graph_name); // single-element graph name path
        pb::append_handle(&mut req, 3, input);
        append_column_indices(&mut req, 4, key_columns)?;
        for &label in labels {
            pb::append_handle(&mut req, 5, label);
        }
        for &definition in definitions {
            pb::append_handle(&mut req, 6, definition);
        }
        // Fields 7 (dynamic label) and 8 (dynamic properties) are left absent.
        self.new_handle(SVC_GRAPH_NODE_TABLE, MID_NEW_GRAPH_NODE_TABLE, &req)
    }

    /// Adds `node_table` to `graph` (`AddNodeTable`, transferring ownership).
    fn register_graph_node_table(&mut self, graph: u64, node_table: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, graph);
        pb::append_handle(&mut req, 2, node_table);
        let resp = self.invoke(SVC_SIMPLE_PROPERTY_GRAPH, MID_ADD_GRAPH_NODE_TABLE, &req)?;
        check_error(&resp)
    }

    /// Builds a `SimpleGraphEdgeTable` over the `input` table, keyed by
    /// `key_columns`, exposing `labels`, defining `definitions`, and connecting
    /// `source` to `destination` (both `SimpleGraphNodeTableReference` handles,
    /// transferred into the edge table). The returned raw handle is transferred
    /// into the graph by
    /// [`register_graph_edge_table`](Self::register_graph_edge_table).
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the SimpleGraphEdgeTable constructor's fields directly"
    )]
    fn new_graph_edge_table(
        &mut self,
        name: &str,
        graph_name: &str,
        input: u64,
        key_columns: &[u32],
        labels: &[u64],
        definitions: &[u64],
        source: u64,
        destination: u64,
    ) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name);
        pb::append_string(&mut req, 2, graph_name); // single-element graph name path
        pb::append_handle(&mut req, 3, input);
        append_column_indices(&mut req, 4, key_columns)?;
        for &label in labels {
            pb::append_handle(&mut req, 5, label);
        }
        for &definition in definitions {
            pb::append_handle(&mut req, 6, definition);
        }
        pb::append_handle(&mut req, 7, source);
        pb::append_handle(&mut req, 8, destination);
        // Fields 9 (dynamic label) and 10 (dynamic properties) are left absent.
        self.new_handle(SVC_GRAPH_EDGE_TABLE, MID_NEW_GRAPH_EDGE_TABLE, &req)
    }

    /// Adds `edge_table` to `graph` (`AddEdgeTable`, transferring ownership).
    fn register_graph_edge_table(&mut self, graph: u64, edge_table: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, graph);
        pb::append_handle(&mut req, 2, edge_table);
        let resp = self.invoke(SVC_SIMPLE_PROPERTY_GRAPH, MID_ADD_GRAPH_EDGE_TABLE, &req)?;
        check_error(&resp)
    }

    /// Registers `graph` into `catalog` under `name` (`AddPropertyGraph2`,
    /// non-owning: the catalog references the graph without freeing it).
    fn register_property_graph(
        &mut self,
        catalog: u64,
        name: &str,
        graph: u64,
    ) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, graph);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_PROPERTY_GRAPH_NAMED, &req)?;
        check_error(&resp)
    }

    /// Registers each table-valued function into `catalog`, returning the created
    /// handles so the caller keeps them alive across the analysis.
    fn add_table_functions(
        &mut self,
        catalog: u64,
        type_factory: u64,
        table_functions: &[TvfDef],
    ) -> Result<Vec<Handle>, Error> {
        // Each TVF contributes at least its relation, result argument, signature,
        // and the TVF object itself, plus one handle per scalar argument (the Vec
        // grows past this hint when arguments are present).
        let mut handles = Vec::with_capacity(table_functions.len().saturating_mul(4));
        for tvf in table_functions {
            self.add_table_function(catalog, type_factory, tvf, &mut handles)?;
        }
        Ok(handles)
    }

    /// Builds a fixed-output-schema TVF from `tvf` and registers it into
    /// `catalog`. Every acquired handle is pushed onto `handles` so it outlives
    /// the analysis (the TVF references its signature and relation by raw
    /// pointer, so they must not be freed before the `AnalyzerOutput`).
    fn add_table_function(
        &mut self,
        catalog: u64,
        type_factory: u64,
        tvf: &TvfDef,
        handles: &mut Vec<Handle>,
    ) -> Result<(), Error> {
        let relation = self.new_tvf_relation(type_factory, &tvf.columns)?;
        // A fixed-output-schema TVF carries its result columns in the relation,
        // so the signature's result is an unconstrained relation type.
        let result_arg = self.new_any_relation_argument_type()?;

        let mut argument_ptrs = Vec::with_capacity(tvf.arguments.len());
        for argument in &tvf.arguments {
            let argument_arg = match argument {
                TvfArgument::Scalar(ty) => {
                    let argument_type = self.build_column_type(type_factory, ty)?;
                    self.new_function_argument_type(argument_type)?
                }
                TvfArgument::AnyRelation => self.new_any_relation_argument_type()?,
                TvfArgument::Relation(columns) => {
                    // The input schema is itself a TVFRelation the argument type
                    // references, so keep it alive alongside the argument.
                    let relation = self.new_tvf_relation(type_factory, columns)?;
                    let argument_arg =
                        self.new_relation_with_schema_argument_type(relation.ptr())?;
                    handles.push(relation);
                    argument_arg
                }
            };
            argument_ptrs.push(argument_arg.ptr());
            handles.push(argument_arg);
        }

        let signature = self.new_function_signature(result_arg.ptr(), &argument_ptrs)?;
        let handle =
            self.new_fixed_output_schema_tvf(&tvf.name, signature.ptr(), relation.ptr())?;
        self.register_table_function(catalog, &tvf.name, handle.ptr())?;

        handles.push(relation);
        handles.push(result_arg);
        handles.push(signature);
        handles.push(handle);
        Ok(())
    }

    /// Builds a `TVFRelation` output schema from `columns`, each contributing a
    /// named column carrying its type handle.
    fn new_tvf_relation(
        &mut self,
        type_factory: u64,
        columns: &[ColumnDef],
    ) -> Result<Handle, Error> {
        let mut req = Vec::new();
        for column in columns {
            let type_handle = self.build_column_type(type_factory, &column.ty)?;
            let mut column_msg = Vec::new();
            pb::append_string(&mut column_msg, FIELD_TVF_COLUMN_NAME, &column.name);
            pb::append_handle(&mut column_msg, FIELD_TVF_COLUMN_TYPE, type_handle);
            pb::append_submessage(&mut req, 1, &column_msg);
        }
        self.acquire_handle(
            SVC_TVF_RELATION,
            MID_NEW_TVF_RELATION,
            &req,
            SVC_TVF_RELATION,
            MID_FREE_TVF_RELATION,
        )
    }

    /// Builds a `FunctionArgumentType` accepting any relation, used as a TVF
    /// signature's result type and for `AnyRelation` table arguments.
    fn new_any_relation_argument_type(&mut self) -> Result<Handle, Error> {
        self.acquire_handle(
            SVC_FUNCTION_ARGUMENT_TYPE,
            MID_NEW_FUNCTION_ARGUMENT_TYPE_ANY_RELATION,
            &[],
            SVC_FUNCTION_ARGUMENT_TYPE,
            MID_FREE_FUNCTION_ARGUMENT_TYPE,
        )
    }

    /// Builds a `FunctionArgumentType` accepting a relation whose schema is
    /// exactly `relation` (extra input columns are not allowed).
    fn new_relation_with_schema_argument_type(&mut self, relation: u64) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, relation);
        pb::append_bool(&mut req, 2, false); // extra_relation_input_columns_allowed
        self.acquire_handle(
            SVC_FUNCTION_ARGUMENT_TYPE,
            MID_NEW_FUNCTION_ARGUMENT_TYPE_RELATION_WITH_SCHEMA,
            &req,
            SVC_FUNCTION_ARGUMENT_TYPE,
            MID_FREE_FUNCTION_ARGUMENT_TYPE,
        )
    }

    /// Builds a `FixedOutputSchemaTVF` named `name`, carrying a single
    /// `signature` and the `relation` describing its output columns.
    fn new_fixed_output_schema_tvf(
        &mut self,
        name: &str,
        signature: u64,
        relation: u64,
    ) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name); // single-element function name path
        pb::append_handle(&mut req, 2, signature); // single-element signatures list
        pb::append_handle(&mut req, 3, relation); // result schema
        // Field 4 (options) is left absent, keeping the defaults.
        self.acquire_handle(
            SVC_FIXED_OUTPUT_SCHEMA_TVF,
            MID_NEW_FIXED_OUTPUT_SCHEMA_TVF,
            &req,
            SVC_FIXED_OUTPUT_SCHEMA_TVF,
            MID_FREE_FIXED_OUTPUT_SCHEMA_TVF,
        )
    }

    /// Registers `tvf` into `catalog` under `name` (non-owning).
    fn register_table_function(&mut self, catalog: u64, name: &str, tvf: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, tvf);
        let resp = self.invoke(
            SVC_SIMPLE_CATALOG,
            MID_ADD_TABLE_VALUED_FUNCTION_NAMED,
            &req,
        )?;
        check_error(&resp)
    }

    /// Registers each named constant into `catalog`, returning the created
    /// `Value` and `SimpleConstant` handles so the caller keeps them alive across
    /// the analysis.
    fn add_constants(
        &mut self,
        catalog: u64,
        constants: &[ConstantDef],
    ) -> Result<Vec<Handle>, Error> {
        let mut handles = Vec::with_capacity(constants.len().saturating_mul(2));
        for constant in constants {
            let value = self.new_value(&constant.value)?;
            let simple = self.new_simple_constant(&constant.name, value.ptr())?;
            self.register_constant(catalog, &constant.name, simple.ptr())?;
            handles.push(value);
            handles.push(simple);
        }
        Ok(handles)
    }

    /// Builds a typed scalar `Value` from `value`.
    fn new_value(&mut self, value: &ConstantValue) -> Result<Handle, Error> {
        let mut req = Vec::new();
        let mid = match *value {
            ConstantValue::Int64(v) => {
                pb::append_int64(&mut req, 1, v);
                MID_NEW_VALUE_INT64
            }
            ConstantValue::Double(v) => {
                pb::append_double(&mut req, 1, v);
                MID_NEW_VALUE_DOUBLE
            }
            ConstantValue::Bool(v) => {
                pb::append_bool(&mut req, 1, v);
                MID_NEW_VALUE_BOOL
            }
            ConstantValue::String(ref v) => {
                pb::append_string(&mut req, 1, v);
                MID_NEW_VALUE_STRING
            }
        };
        self.acquire_handle(SVC_VALUE, mid, &req, SVC_VALUE, MID_FREE_VALUE)
    }

    /// Builds a `SimpleConstant` named `name` carrying `value`.
    fn new_simple_constant(&mut self, name: &str, value: u64) -> Result<Handle, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, name);
        pb::append_handle(&mut req, 2, value);
        self.acquire_handle(
            SVC_SIMPLE_CONSTANT,
            MID_NEW_SIMPLE_CONSTANT,
            &req,
            SVC_SIMPLE_CONSTANT,
            MID_FREE_SIMPLE_CONSTANT,
        )
    }

    /// Registers `constant` into `catalog` under `name` (non-owning).
    fn register_constant(&mut self, catalog: u64, name: &str, constant: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, constant);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_CONSTANT_NAMED, &req)?;
        check_error(&resp)
    }

    /// Declares each typed query parameter on the `AnalyzerOptions`. The parameter
    /// types are owned by `type_factory` (which outlives the analysis), so no
    /// handles need to be retained here.
    fn add_query_parameters(
        &mut self,
        options: u64,
        type_factory: u64,
        parameters: &[QueryParameter],
    ) -> Result<(), Error> {
        for parameter in parameters {
            let type_handle = self.build_column_type(type_factory, &parameter.ty)?;
            self.add_query_parameter(options, &parameter.name, type_handle)?;
        }
        Ok(())
    }

    /// Declares a single named query parameter of type `type_handle` on `options`.
    fn add_query_parameter(
        &mut self,
        options: u64,
        name: &str,
        type_handle: u64,
    ) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, options);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, type_handle);
        let resp = self.invoke(SVC_ANALYZER_OPTIONS, MID_ADD_QUERY_PARAMETER, &req)?;
        check_error(&resp)
    }

    /// Registers GoogleSQL's builtin functions and types (with default language
    /// options) into `catalog`, so operators like `+` and standard functions
    /// resolve during analysis.
    fn add_builtin_functions(&mut self, catalog: u64) -> Result<(), Error> {
        // Enable the maximum language feature set so builtins match the features
        // the parser and analyzer accept (e.g. the `QUALIFY` clause).
        let language = self.language_options()?;
        self.add_builtins_with_language(catalog, language)
    }

    /// Invokes `AddBuiltinFunctionsAndTypes` with a `BuiltinFunctionOptions`
    /// built from the given `LanguageOptions` handle.
    fn add_builtins_with_language(&mut self, catalog: u64, language: u64) -> Result<(), Error> {
        let mut builtin_options = Vec::new();
        pb::append_handle(
            &mut builtin_options,
            FIELD_BUILTIN_OPTIONS_LANGUAGE,
            language,
        );

        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_submessage(&mut req, 2, &builtin_options);

        let resp = self.invoke(
            SVC_SIMPLE_CATALOG,
            MID_ADD_BUILTIN_FUNCTIONS_AND_TYPES,
            &req,
        )?;
        check_error(&resp)
    }

    /// Invokes the analyzer entry point named by `call.mid` (`AnalyzeStatement`
    /// or `AnalyzeExpression`), runs `extract` on the resolved output, then
    /// releases the resulting `AnalyzerOutput` handle. Both entry points share the
    /// same request shape (sql, options, catalog, type factory) and return an
    /// `AnalyzerOutput` in field 2.
    fn analyze<T>(
        &mut self,
        sql: &str,
        call: AnalyzerCall,
        catalog: u64,
        type_factory: u64,
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, sql);
        pb::append_handle(&mut req, 2, call.options);
        pb::append_handle(&mut req, 3, catalog);
        pb::append_handle(&mut req, 4, type_factory);
        let resp = self.invoke(SVC_ANALYZER, call.mid, &req)?;
        check_error(&resp)?;

        let output_ptr = pb::read_handle_at_field(&resp, 2);
        if output_ptr == 0 {
            return extract(self, 0);
        }

        // Run the extractor while the output (and the catalog/type factory that
        // own its nodes and types) is still alive; any nodes it visits are
        // borrowed pointers into that tree, so none are freed here.
        //
        // An owned `AnalyzerOutput` (statement/expression analysis) is registered
        // for a free by the top-level flush. `AnalyzeType` instead returns a
        // `Type` borrowed from the still-live type factory, so its handle is
        // passed through without a free.
        if call.frees_output {
            let output =
                self.register_free(SVC_ANALYZER_OUTPUT, MID_FREE_ANALYZER_OUTPUT, output_ptr);
            extract(self, output.ptr())
        } else {
            extract(self, output_ptr)
        }
    }

    /// Analyzes each statement of a possibly multi-statement `sql` in turn via
    /// `AnalyzeNextStatement`, running `extract` on each statement's
    /// `AnalyzerOutput` and collecting the results.
    ///
    /// A `ParseResumeLocation` carries the byte offset between calls; each call
    /// advances it past one statement (and its separating semicolon). The loop
    /// stops once the resume location reaches the end of the input or a call
    /// yields no statement (only trailing whitespace or comments remain). Each
    /// statement's owned `AnalyzerOutput` is freed after its extractor runs. A
    /// statement that fails analysis surfaces its error and ends the run.
    fn analyze_each<T>(
        &mut self,
        sql: &str,
        call: AnalyzerCall,
        catalog: u64,
        type_factory: u64,
        mut extract: impl FnMut(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        // `AnalyzeNextStatement` errors if asked to parse a stretch of only
        // whitespace, so the loop stops once the offset reaches the last
        // non-whitespace byte. The resume location still gets the full input for
        // correct byte offsets; only the termination bound is trimmed.
        let effective_len = i32::try_from(sql.trim_end().len())
            .map_err(|_| Error::Protocol("sql too long".into()))?;
        let mut resume_req = Vec::new();
        pb::append_string(&mut resume_req, 1, sql);
        let resume = self.acquire_handle(
            SVC_PARSE_RESUME_LOCATION,
            MID_NEW_RESUME_LOCATION_FROM_STRING,
            &resume_req,
            SVC_PARSE_RESUME_LOCATION,
            MID_FREE_RESUME_LOCATION,
        )?;

        let mut results = Vec::new();
        loop {
            let position = self.resume_byte_position(resume.ptr())?;
            // Nothing left to parse once the offset reaches the trailing
            // whitespace after the last statement.
            if position >= effective_len {
                break;
            }
            let output_ptr =
                self.analyze_next_statement(resume.ptr(), call.options, catalog, type_factory)?;
            // A null output with an OK status means only trailing whitespace or
            // comments followed the last statement, so the script is exhausted.
            if output_ptr == 0 {
                break;
            }
            let output =
                self.register_free(SVC_ANALYZER_OUTPUT, MID_FREE_ANALYZER_OUTPUT, output_ptr);
            results.push(extract(self, output.ptr())?);
            // The call always advances past a produced statement; guard against a
            // stuck offset so a non-advancing location cannot loop forever.
            if self.resume_byte_position(resume.ptr())? <= position {
                break;
            }
        }
        Ok(results)
    }

    /// Invokes `AnalyzeNextStatement`, returning the next statement's
    /// `AnalyzerOutput` handle (0 when the input holds no further statement).
    fn analyze_next_statement(
        &mut self,
        resume: u64,
        options: u64,
        catalog: u64,
        type_factory: u64,
    ) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, resume);
        pb::append_handle(&mut req, 2, options);
        pb::append_handle(&mut req, 3, catalog);
        pb::append_handle(&mut req, 4, type_factory);
        let resp = self.invoke(SVC_ANALYZER, MID_ANALYZE_NEXT_STATEMENT, &req)?;
        check_error(&resp)?;
        Ok(pb::read_handle_at_field(&resp, 2))
    }

    /// Reads a `ParseResumeLocation`'s current byte offset into the input.
    fn resume_byte_position(&mut self, resume: u64) -> Result<i32, Error> {
        let resp = self.invoke_handle(
            SVC_PARSE_RESUME_LOCATION,
            MID_RESUME_LOCATION_BYTE_POSITION,
            resume,
        )?;
        check_error(&resp)?;
        Ok(pb::read_int32_at_field(&resp, 1).unwrap_or(0))
    }
}
