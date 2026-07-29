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

use crate::error::Error;
use crate::pb;
use crate::resolved::{OutputColumn, ResolvedNode, TableRef};
use crate::runtime::{Handle, Module};

const SVC_ANALYZER: i32 = 0;
const MID_ANALYZE_STATEMENT: i32 = 2;

const SVC_ANALYZER_OPTIONS: i32 = 554;
const MID_NEW_ANALYZER_OPTIONS: i32 = 1;
const MID_SET_PRUNE_UNUSED_COLUMNS: i32 = 80;
const MID_FREE_ANALYZER_OPTIONS: i32 = 86;

const SVC_TYPE_FACTORY: i32 = 1419;
const MID_NEW_TYPE_FACTORY: i32 = 0;
const MID_FREE_TYPE_FACTORY: i32 = 62;
const MID_GET_BOOL: i32 = 42;
const MID_GET_DOUBLE: i32 = 46;
const MID_GET_INT64: i32 = 50;
const MID_GET_STRING: i32 = 54;

const SVC_SIMPLE_CATALOG: i32 = 1347;
const MID_NEW_SIMPLE_CATALOG: i32 = 0;
const MID_ADD_BUILTIN_FUNCTIONS_AND_TYPES: i32 = 3;
const MID_ADD_TABLE_NAMED: i32 = 72;
const MID_FREE_SIMPLE_CATALOG: i32 = 114;

const SVC_SIMPLE_TABLE: i32 = 1380;
const MID_NEW_SIMPLE_TABLE: i32 = 0;
const MID_ADD_COLUMN_OWNED: i32 = 4;
const MID_FREE_SIMPLE_TABLE: i32 = 27;

const SVC_SIMPLE_COLUMN: i32 = 1350;
const MID_NEW_SIMPLE_COLUMN: i32 = 0;
const MID_FREE_SIMPLE_COLUMN: i32 = 10;

const SVC_LANGUAGE_OPTIONS: i32 = 678;
const MID_NEW_LANGUAGE_OPTIONS: i32 = 0;
const MID_FREE_LANGUAGE_OPTIONS: i32 = 29;

/// Field of `BuiltinFunctionOptions` that carries the `LanguageOptions` handle.
const FIELD_BUILTIN_OPTIONS_LANGUAGE: u32 = 4;

const SVC_ANALYZER_OUTPUT: i32 = 558;
const MID_FREE_ANALYZER_OUTPUT: i32 = 11;

/// A column type that a user table can expose to the analyzer.
///
/// Each variant maps to a `TypeFactory` accessor for the corresponding
/// GoogleSQL scalar type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    /// 64-bit signed integer (`INT64`).
    Int64,
    /// Variable-length string (`STRING`).
    String,
    /// Boolean (`BOOL`).
    Bool,
    /// Double-precision float (`FLOAT64`).
    Float64,
}

impl ColumnType {
    /// The `TypeFactory` method id that returns this type's handle.
    const fn type_factory_mid(self) -> i32 {
        match self {
            Self::Int64 => MID_GET_INT64,
            Self::String => MID_GET_STRING,
            Self::Bool => MID_GET_BOOL,
            Self::Float64 => MID_GET_DOUBLE,
        }
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

impl Module {
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
        self.run_analysis(sql, tables, false, |_, _| Ok(()))
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
        self.run_analysis(sql, tables, false, Self::output_columns)
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
        self.run_analysis(sql, tables, true, Self::referenced_tables_of)
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
        self.run_analysis(sql, tables, false, Self::resolved_tree_of)
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
        tables: &[TableDef],
        prune_columns: bool,
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
            module.configure_options(options.ptr(), prune_columns)?;
            module.analyze_with_options(sql, options.ptr(), tables, extract)
        })
    }

    /// Applies analysis options, enabling column pruning when requested so table
    /// scans expose only referenced columns.
    fn configure_options(&mut self, options: u64, prune_columns: bool) -> Result<(), Error> {
        if !prune_columns {
            return Ok(());
        }
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, options);
        pb::append_bool(&mut req, 2, true);
        let resp = self.invoke(SVC_ANALYZER_OPTIONS, MID_SET_PRUNE_UNUSED_COLUMNS, &req)?;
        check_error(&resp)
    }

    /// Builds the `TypeFactory` handle and runs the analysis against it. The
    /// factory outlives the analysis (it owns the resolved output's types) and is
    /// freed by the top-level [`flush_frees`](Module::flush_frees).
    fn analyze_with_options<T>(
        &mut self,
        sql: &str,
        options: u64,
        tables: &[TableDef],
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let type_factory = self.acquire_handle(
            SVC_TYPE_FACTORY,
            MID_NEW_TYPE_FACTORY,
            &[],
            SVC_TYPE_FACTORY,
            MID_FREE_TYPE_FACTORY,
        )?;
        self.analyze_with_catalog(sql, options, type_factory.ptr(), tables, extract)
    }

    /// Builds a `SimpleCatalog` handle over `type_factory`, populates it with
    /// `tables`, and runs the analysis. The catalog outlives the analysis (it
    /// owns the resolved output's nodes) and is freed by the top-level
    /// [`flush_frees`](Module::flush_frees).
    fn analyze_with_catalog<T>(
        &mut self,
        sql: &str,
        options: u64,
        type_factory: u64,
        tables: &[TableDef],
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
        self.populate_and_analyze(sql, options, catalog.ptr(), type_factory, tables, extract)
    }

    /// Registers `tables` into `catalog` and runs the analysis. The created
    /// `SimpleTable` handles (which own their columns) stay alive until `extract`
    /// returns and are freed by the top-level [`flush_frees`](Module::flush_frees).
    fn populate_and_analyze<T>(
        &mut self,
        sql: &str,
        options: u64,
        catalog: u64,
        type_factory: u64,
        tables: &[TableDef],
        extract: impl FnOnce(&mut Self, u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // Bound (not `_`) so the SimpleTable handles stay alive across `analyze`
        // and enqueue their frees only after it returns: dropping them earlier
        // would order their frees ahead of the AnalyzerOutput that references them.
        let _table_handles = self.add_tables(catalog, type_factory, tables)?;
        self.analyze(sql, options, catalog, type_factory, extract)
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
            let type_handle = self.new_handle(
                SVC_TYPE_FACTORY,
                column.ty.type_factory_mid(),
                &pb::handle_arg(type_factory),
            )?;

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

    /// Registers `table` under `name` in `catalog` (non-owning).
    fn register_table(&mut self, catalog: u64, name: &str, table: u64) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, catalog);
        pb::append_string(&mut req, 2, name);
        pb::append_handle(&mut req, 3, table);
        let resp = self.invoke(SVC_SIMPLE_CATALOG, MID_ADD_TABLE_NAMED, &req)?;
        check_error(&resp)
    }

    /// Registers GoogleSQL's builtin functions and types (with default language
    /// options) into `catalog`, so operators like `+` and standard functions
    /// resolve during analysis.
    fn add_builtin_functions(&mut self, catalog: u64) -> Result<(), Error> {
        let language = self.acquire_handle(
            SVC_LANGUAGE_OPTIONS,
            MID_NEW_LANGUAGE_OPTIONS,
            &[],
            SVC_LANGUAGE_OPTIONS,
            MID_FREE_LANGUAGE_OPTIONS,
        )?;
        // `language` is consumed here; the top-level flush frees it afterwards.
        self.add_builtins_with_language(catalog, language.ptr())
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

/// Converts an error in field 15 of the response into [`Error::GoogleSql`].
fn check_error(resp: &[u8]) -> Result<(), Error> {
    pb::extract_error(resp).map_or(Ok(()), |message| Err(Error::GoogleSql(message)))
}
