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
const MID_ANALYZE_STATEMENT: i32 = 2;

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

const SVC_SIMPLE_CATALOG: i32 = 1347;
const MID_NEW_SIMPLE_CATALOG: i32 = 0;
const MID_ADD_BUILTIN_FUNCTIONS_AND_TYPES: i32 = 3;
const MID_ADD_FUNCTION: i32 = 10;
const MID_ADD_TABLE_NAMED: i32 = 72;
const MID_ADD_CATALOG_NAMED: i32 = 4;
const MID_ADD_CONSTANT_NAMED: i32 = 8;
const MID_ADD_TABLE_VALUED_FUNCTION_NAMED: i32 = 73;
const MID_FREE_SIMPLE_CATALOG: i32 = 114;

const SVC_SIMPLE_TABLE: i32 = 1380;
const MID_NEW_SIMPLE_TABLE: i32 = 0;
const MID_ADD_COLUMN_OWNED: i32 = 4;
const MID_FREE_SIMPLE_TABLE: i32 = 27;

const SVC_SIMPLE_COLUMN: i32 = 1350;
const MID_NEW_SIMPLE_COLUMN: i32 = 0;
const MID_FREE_SIMPLE_COLUMN: i32 = 10;

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

const SVC_TVF_RELATION: i32 = 1400;
const MID_NEW_TVF_RELATION: i32 = 0;
const MID_FREE_TVF_RELATION: i32 = 10;

const SVC_FIXED_OUTPUT_SCHEMA_TVF: i32 = 632;
const MID_NEW_FIXED_OUTPUT_SCHEMA_TVF: i32 = 2;
const MID_FREE_FIXED_OUTPUT_SCHEMA_TVF: i32 = 5;

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
            Self::Array(_) | Self::Struct(_) | Self::Range(_) | Self::Map(_, _) => return None,
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

/// The declarations made visible to the analyzer for a query.
///
/// Bundles the [`tables`](Self::tables), [`functions`](Self::functions),
/// [`constants`](Self::constants), [`parameters`](Self::parameters),
/// [`table_functions`](Self::table_functions), and nested
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
        }
    }
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
            module.analyze_with_options(sql, options.ptr(), catalog, extract)
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
    fn analyze_with_options<T>(
        &mut self,
        sql: &str,
        options: u64,
        catalog: CatalogContents,
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let type_factory = self.acquire_handle(
            SVC_TYPE_FACTORY,
            MID_NEW_TYPE_FACTORY,
            &[],
            SVC_TYPE_FACTORY,
            MID_FREE_TYPE_FACTORY,
        )?;
        self.analyze_with_catalog(sql, options, type_factory.ptr(), catalog, extract)
    }

    /// Builds a `SimpleCatalog` handle over `type_factory`, populates it with the
    /// catalog contents, and runs the analysis. The catalog outlives the analysis
    /// (it owns the resolved output's nodes) and is freed by the top-level
    /// [`flush_frees`](Module::flush_frees).
    fn analyze_with_catalog<T>(
        &mut self,
        sql: &str,
        options: u64,
        type_factory: u64,
        contents: CatalogContents,
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
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
        self.populate_and_analyze(sql, options, catalog.ptr(), type_factory, contents, extract)
    }

    /// Registers a catalog's declarations and runs the analysis. Every handle
    /// created while populating the catalog stays alive until `extract` returns
    /// and is freed by the top-level [`flush_frees`](Module::flush_frees).
    fn populate_and_analyze<T>(
        &mut self,
        sql: &str,
        options: u64,
        catalog: u64,
        type_factory: u64,
        contents: CatalogContents,
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // Bound (not `_`) so these handles stay alive across `analyze` and enqueue
        // their frees only after it returns: dropping them earlier would order
        // their frees ahead of the AnalyzerOutput that references them.
        let _handles = self.populate_catalog(catalog, type_factory, contents)?;
        // Query parameters are an analysis-wide `AnalyzerOptions` setting rather
        // than a catalog declaration, so only the top-level contents supply them.
        self.add_query_parameters(options, type_factory, contents.parameters)?;
        self.analyze(sql, options, catalog, type_factory, extract)
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

        let mut argument_ptrs = Vec::with_capacity(function.arguments.len());
        for argument in &function.arguments {
            let argument_type = self.build_column_type(type_factory, argument)?;
            let argument_arg = self.new_function_argument_type(argument_type)?;
            argument_ptrs.push(argument_arg.ptr());
            handles.push(argument_arg);
        }

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

    /// Invokes `AnalyzeStatement`, runs `extract` on the resolved output, then
    /// releases the resulting `AnalyzerOutput` handle.
    fn analyze<T>(
        &mut self,
        sql: &str,
        options: u64,
        catalog: u64,
        type_factory: u64,
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, sql);
        pb::append_handle(&mut req, 2, options);
        pb::append_handle(&mut req, 3, catalog);
        pb::append_handle(&mut req, 4, type_factory);
        let resp = self.invoke(SVC_ANALYZER, MID_ANALYZE_STATEMENT, &req)?;
        check_error(&resp)?;

        let output_ptr = pb::read_handle_at_field(&resp, 2);
        if output_ptr == 0 {
            return extract(self, 0);
        }

        // Run the extractor while the output (and the catalog/type factory that
        // own its nodes and types) is still alive; the output is freed by the
        // top-level flush afterwards. Any nodes the extractor visits are borrowed
        // pointers into that tree, so none are freed here.
        let output = self.register_free(SVC_ANALYZER_OUTPUT, MID_FREE_ANALYZER_OUTPUT, output_ptr);
        extract(self, output.ptr())
    }
}
