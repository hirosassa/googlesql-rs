//! High-level API for the GoogleSQL parser.
//!
//! Call chain (svc/mid values from `docs/SPIKE.md`):
//! NewParserOptions(699,0) → ParseStatement(0,10) → ParserOutput.Node(700,3)
//! → Unparse(0,12). All acquired handles are released after use.
//!
//! Semicolon-separated scripts run through a second chain that drives
//! ParseNextStatement(0,8) with a ParseResumeLocation(695,2) until the
//! `at_end_of_input` flag is set.

use crate::ast::AstNode;
use crate::error::{Error, SqlError, check_error};
use crate::pb;
use crate::runtime::Module;

const SVC_PARSER: i32 = 0;
const MID_PARSE_STATEMENT: i32 = 10;
const MID_PARSE_NEXT_STATEMENT: i32 = 8;
const MID_UNPARSE: i32 = 12;

const SVC_PARSER_OPTIONS: i32 = 699;
const MID_NEW_PARSER_OPTIONS: i32 = 0;
const MID_SET_PARSER_LANGUAGE_OPTIONS: i32 = 11;
const MID_FREE_PARSER_OPTIONS: i32 = 12;

const SVC_PARSER_OUTPUT: i32 = 700;
const MID_PARSER_OUTPUT_NODE: i32 = 3;
const MID_FREE_PARSER_OUTPUT: i32 = 9;

const SVC_PARSE_RESUME_LOCATION: i32 = 695;
const MID_NEW_PARSE_RESUME_LOCATION_FROM_STRING: i32 = 2;
const MID_FREE_PARSE_RESUME_LOCATION: i32 = 14;

/// `ParseNextStatement` response field carrying the `at_end_of_input` bool: it
/// is set on the call that returns the final statement, signalling that the
/// input is exhausted and no further call should be made.
const FIELD_PARSE_NEXT_AT_END: u32 = 3;

/// A parsed SQL statement.
///
/// Holds the normalized (unparsed) SQL string and a self-contained AST tree.
#[derive(Debug, Clone)]
pub struct ParsedStatement {
    canonical_sql: String,
    root: AstNode,
}

impl ParsedStatement {
    /// The normalized canonical SQL string.
    pub fn canonical_sql(&self) -> &str {
        &self.canonical_sql
    }

    /// The root node of the AST.
    pub const fn root(&self) -> &AstNode {
        &self.root
    }
}

/// The outcome of parsing a semicolon-separated script.
///
/// GoogleSQL parses one statement at a time and cannot recover past a syntax
/// error, so [`statements`](Self::statements) holds every statement parsed
/// before parsing halted and [`error`](Self::error) holds the syntax error that
/// stopped it, if any.
#[derive(Debug, Clone)]
pub struct ParsedStatements {
    statements: Vec<ParsedStatement>,
    error: Option<SqlError>,
}

impl ParsedStatements {
    /// The statements parsed successfully, in source order.
    pub fn statements(&self) -> &[ParsedStatement] {
        &self.statements
    }

    /// The syntax error that halted parsing, or `None` if the whole script parsed.
    ///
    /// Like [`parse_statement`](Module::parse_statement) errors, this carries no
    /// line/column: the wasm ABI reports parser errors as a plain string with no
    /// position payload.
    pub const fn error(&self) -> Option<&SqlError> {
        self.error.as_ref()
    }

    /// Whether the entire script parsed without a syntax error.
    pub const fn is_complete(&self) -> bool {
        self.error.is_none()
    }

    /// Consumes the result, returning the statements parsed before any error.
    pub fn into_statements(self) -> Vec<ParsedStatement> {
        self.statements
    }
}

impl Module {
    /// Parses a SQL statement and returns the normalized result.
    ///
    /// Returns [`Error::GoogleSql`] on a syntax error.
    ///
    /// Every wasm-side handle acquired during the parse is an RAII `Handle`
    /// that enqueues its own free on drop; the enclosing `with_frees`
    /// releases them all, whether the parse succeeded or failed.
    pub fn parse_statement(&mut self, sql: &str) -> Result<ParsedStatement, Error> {
        self.with_frees(|module| {
            let options = module.acquire_handle(
                SVC_PARSER_OPTIONS,
                MID_NEW_PARSER_OPTIONS,
                &[],
                SVC_PARSER_OPTIONS,
                MID_FREE_PARSER_OPTIONS,
            )?;
            // Enable the maximum language feature set so gated syntax such as the
            // `QUALIFY` clause is accepted.
            let language = module.max_language_options()?;
            module.set_parser_language_options(options.ptr(), language)?;
            module.parse_with_options(sql, options.ptr())
        })
    }

    /// Parses a semicolon-separated script into its constituent statements.
    ///
    /// Statements are parsed one at a time via `ParseNextStatement`, driven by a
    /// `ParseResumeLocation` that tracks how far the input has been consumed.
    /// The same maximum-language-feature `ParserOptions` as
    /// [`parse_statement`](Self::parse_statement) applies to every statement, so
    /// gated syntax such as the `QUALIFY` clause is accepted.
    ///
    /// GoogleSQL halts at the first syntax error and cannot resume past it, so a
    /// malformed statement stops parsing: the returned [`ParsedStatements`] then
    /// holds every statement parsed before the error plus the error itself.
    ///
    /// The outer [`Error`] is reserved for infrastructure failures (a wasm or
    /// protocol fault); a GoogleSQL syntax error is reported through
    /// [`ParsedStatements::error`], never as an outer `Err`.
    pub fn parse_statements(&mut self, sql: &str) -> Result<ParsedStatements, Error> {
        self.with_frees(|module| {
            let options = module.acquire_handle(
                SVC_PARSER_OPTIONS,
                MID_NEW_PARSER_OPTIONS,
                &[],
                SVC_PARSER_OPTIONS,
                MID_FREE_PARSER_OPTIONS,
            )?;
            // Enable the maximum language feature set so gated syntax such as the
            // `QUALIFY` clause is accepted for every statement in the script.
            let language = module.max_language_options()?;
            module.set_parser_language_options(options.ptr(), language)?;

            let mut resume_req = Vec::new();
            pb::append_string(&mut resume_req, 1, sql);
            let resume = module.acquire_handle(
                SVC_PARSE_RESUME_LOCATION,
                MID_NEW_PARSE_RESUME_LOCATION_FROM_STRING,
                &resume_req,
                SVC_PARSE_RESUME_LOCATION,
                MID_FREE_PARSE_RESUME_LOCATION,
            )?;

            let mut statements = Vec::new();
            let error = loop {
                let mut req = Vec::new();
                pb::append_handle(&mut req, 1, resume.ptr());
                pb::append_handle(&mut req, 2, options.ptr());
                let resp = module.invoke(SVC_PARSER, MID_PARSE_NEXT_STATEMENT, &req)?;
                // A syntax error halts the script: GoogleSQL cannot resume past it.
                if let Some(message) = pb::extract_error(&resp) {
                    break Some(message.into());
                }
                let output_ptr = pb::read_handle_at_field(&resp, 2);
                if output_ptr == 0 {
                    break None;
                }
                let output =
                    module.register_free(SVC_PARSER_OUTPUT, MID_FREE_PARSER_OUTPUT, output_ptr);
                let (canonical_sql, root) = module.build_from_output(output.ptr())?;
                statements.push(ParsedStatement {
                    canonical_sql,
                    root,
                });
                // The final statement carries `at_end_of_input`; stopping here
                // avoids an extra call that would report a spurious "Unexpected
                // end of statement" past the exhausted input.
                if pb::read_bool_at_field(&resp, FIELD_PARSE_NEXT_AT_END) {
                    break None;
                }
            };

            Ok(ParsedStatements { statements, error })
        })
    }

    /// Wires a `LanguageOptions` handle into a `ParserOptions` handle.
    fn set_parser_language_options(
        &mut self,
        options_ptr: u64,
        language_ptr: u64,
    ) -> Result<(), Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, options_ptr);
        pb::append_handle(&mut req, 2, language_ptr);
        let resp = self.invoke(SVC_PARSER_OPTIONS, MID_SET_PARSER_LANGUAGE_OPTIONS, &req)?;
        check_error(&resp)
    }

    /// Parses using a pre-built `ParserOptions` handle and produces the canonical SQL.
    fn parse_with_options(
        &mut self,
        sql: &str,
        options_ptr: u64,
    ) -> Result<ParsedStatement, Error> {
        let mut req = Vec::new();
        pb::append_string(&mut req, 1, sql);
        pb::append_handle(&mut req, 2, options_ptr);
        let resp = self.invoke(SVC_PARSER, MID_PARSE_STATEMENT, &req)?;
        check_error(&resp)?;
        let output_ptr = pb::read_handle_at_field(&resp, 2);
        if output_ptr == 0 {
            return Err(Error::Protocol("ParseStatement returned null".into()));
        }
        // The ParserOutput handle (which also owns the AST arena) is freed by the
        // top-level `flush_frees` after `build_from_output` has read the tree.
        let output = self.register_free(SVC_PARSER_OUTPUT, MID_FREE_PARSER_OUTPUT, output_ptr);

        let (canonical_sql, root) = self.build_from_output(output.ptr())?;
        Ok(ParsedStatement {
            canonical_sql,
            root,
        })
    }

    /// Extracts the AST root from a `ParserOutput` handle and builds the canonical SQL and AST tree.
    fn build_from_output(&mut self, output_ptr: u64) -> Result<(String, AstNode), Error> {
        let node_resp = self.invoke(
            SVC_PARSER_OUTPUT,
            MID_PARSER_OUTPUT_NODE,
            &pb::handle_arg(output_ptr),
        )?;
        check_error(&node_resp)?;
        let node_ptr = pb::read_handle_at_field(&node_resp, 1);
        if node_ptr == 0 {
            return Err(Error::Protocol("ParserOutput.Node returned null".into()));
        }

        let unparsed = self.invoke(SVC_PARSER, MID_UNPARSE, &pb::handle_arg(node_ptr))?;
        check_error(&unparsed)?;
        let canonical = pb::read_string_at_field(&unparsed, 1)
            .ok_or_else(|| Error::Protocol("Unparse returned no string".into()))?;

        let root = self.build_ast(node_ptr)?;
        Ok((canonical, root))
    }
}
