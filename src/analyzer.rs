//! High-level API for the GoogleSQL analyzer.
//!
//! Call chain (svc/mid values measured from the wazero reference glue):
//! NewAnalyzerOptions2(554,1) + NewTypeFactory(1419,0) + NewSimpleCatalog(1347,0)
//! → AnalyzeStatement(0,2). All acquired handles are released after use.
//!
//! Phase 1 resolves against an empty catalog and reports success or failure only;
//! user-defined tables and typed access to the resolved AST come later.

use crate::error::Error;
use crate::pb;
use crate::runtime::Module;

const SVC_ANALYZER: i32 = 0;
const MID_ANALYZE_STATEMENT: i32 = 2;

const SVC_ANALYZER_OPTIONS: i32 = 554;
const MID_NEW_ANALYZER_OPTIONS: i32 = 1;
const MID_FREE_ANALYZER_OPTIONS: i32 = 86;

const SVC_TYPE_FACTORY: i32 = 1419;
const MID_NEW_TYPE_FACTORY: i32 = 0;
const MID_FREE_TYPE_FACTORY: i32 = 62;

const SVC_SIMPLE_CATALOG: i32 = 1347;
const MID_NEW_SIMPLE_CATALOG: i32 = 0;
const MID_ADD_BUILTIN_FUNCTIONS_AND_TYPES: i32 = 3;
const MID_FREE_SIMPLE_CATALOG: i32 = 114;

const SVC_LANGUAGE_OPTIONS: i32 = 678;
const MID_NEW_LANGUAGE_OPTIONS: i32 = 0;
const MID_FREE_LANGUAGE_OPTIONS: i32 = 29;

/// Field of `BuiltinFunctionOptions` that carries the `LanguageOptions` handle.
const FIELD_BUILTIN_OPTIONS_LANGUAGE: u32 = 4;

const SVC_ANALYZER_OUTPUT: i32 = 558;
const MID_FREE_ANALYZER_OUTPUT: i32 = 11;

impl Module {
    /// Analyzes a SQL statement against an empty catalog.
    ///
    /// Performs type inference and name resolution. Returns [`Error::GoogleSql`]
    /// on a syntax error or when a referenced name cannot be resolved (which, with
    /// an empty catalog, includes any table reference).
    pub fn analyze_statement(&mut self, sql: &str) -> Result<(), Error> {
        let options = self.new_handle(SVC_ANALYZER_OPTIONS, MID_NEW_ANALYZER_OPTIONS, &[])?;

        // Free the options handle regardless of analysis success or failure.
        let result = self.analyze_with_options(sql, options);
        let freed = self.invoke(
            SVC_ANALYZER_OPTIONS,
            MID_FREE_ANALYZER_OPTIONS,
            &pb::handle_arg(options),
        );
        result?;
        freed?;
        Ok(())
    }

    /// Builds the `TypeFactory` handle and runs the analysis against it,
    /// freeing the factory regardless of success or failure.
    fn analyze_with_options(&mut self, sql: &str, options: u64) -> Result<(), Error> {
        let type_factory = self.new_handle(SVC_TYPE_FACTORY, MID_NEW_TYPE_FACTORY, &[])?;

        let result = self.analyze_with_catalog(sql, options, type_factory);
        let freed = self.invoke(
            SVC_TYPE_FACTORY,
            MID_FREE_TYPE_FACTORY,
            &pb::handle_arg(type_factory),
        );
        result?;
        freed?;
        Ok(())
    }

    /// Builds a `SimpleCatalog` handle over `type_factory` and runs the analysis,
    /// freeing the catalog regardless of success or failure.
    fn analyze_with_catalog(
        &mut self,
        sql: &str,
        options: u64,
        type_factory: u64,
    ) -> Result<(), Error> {
        let mut catalog_req = Vec::new();
        pb::append_string(&mut catalog_req, 1, "");
        pb::append_handle(&mut catalog_req, 2, type_factory);
        let catalog = self.new_handle(SVC_SIMPLE_CATALOG, MID_NEW_SIMPLE_CATALOG, &catalog_req)?;

        let analyzed = self
            .add_builtin_functions(catalog)
            .and_then(|()| self.analyze(sql, options, catalog, type_factory));
        let freed = self.invoke(
            SVC_SIMPLE_CATALOG,
            MID_FREE_SIMPLE_CATALOG,
            &pb::handle_arg(catalog),
        );
        analyzed?;
        freed?;
        Ok(())
    }

    /// Registers GoogleSQL's builtin functions and types (with default language
    /// options) into `catalog`, so operators like `+` and standard functions
    /// resolve during analysis.
    fn add_builtin_functions(&mut self, catalog: u64) -> Result<(), Error> {
        let language = self.new_handle(SVC_LANGUAGE_OPTIONS, MID_NEW_LANGUAGE_OPTIONS, &[])?;

        // Free the LanguageOptions handle regardless of registration success.
        let added = self.add_builtins_with_language(catalog, language);
        let freed = self.invoke(
            SVC_LANGUAGE_OPTIONS,
            MID_FREE_LANGUAGE_OPTIONS,
            &pb::handle_arg(language),
        );
        added?;
        freed?;
        Ok(())
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

    /// Invokes `AnalyzeStatement` and releases the resulting `AnalyzerOutput` handle.
    fn analyze(
        &mut self,
        sql: &str,
        options: u64,
        catalog: u64,
        type_factory: u64,
    ) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, sql);
        pb::append_handle(&mut req, 2, options);
        pb::append_handle(&mut req, 3, catalog);
        pb::append_handle(&mut req, 4, type_factory);
        let resp = self.invoke(SVC_ANALYZER, MID_ANALYZE_STATEMENT, &req)?;
        check_error(&resp)?;

        // The resolved output is not surfaced yet (Phase 1); free it immediately.
        let output = pb::read_handle_at_field(&resp, 2);
        if output != 0 {
            self.invoke(
                SVC_ANALYZER_OUTPUT,
                MID_FREE_ANALYZER_OUTPUT,
                &pb::handle_arg(output),
            )?;
        }
        Ok(())
    }

    /// Invokes a constructor and returns the non-null handle from response field 1.
    fn new_handle(&mut self, svc: i32, mid: i32, req: &[u8]) -> Result<u64, Error> {
        let resp = self.invoke(svc, mid, req)?;
        check_error(&resp)?;
        let handle = pb::read_handle_at_field(&resp, 1);
        if handle == 0 {
            return Err(Error::GoogleSql(format!(
                "constructor w_{svc}_{mid} returned null"
            )));
        }
        Ok(handle)
    }
}

/// Converts an error in field 15 of the response into [`Error::GoogleSql`].
fn check_error(resp: &[u8]) -> Result<(), Error> {
    pb::extract_error(resp).map_or(Ok(()), |message| Err(Error::GoogleSql(message)))
}
