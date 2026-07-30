//! High-level API for the GoogleSQL parser.
//!
//! Call chain (svc/mid values from `docs/SPIKE.md`):
//! NewParserOptions(699,0) → ParseStatement(0,10) → ParserOutput.Node(700,3)
//! → Unparse(0,12). All acquired handles are released after use.

use crate::ast::AstNode;
use crate::error::{Error, check_error};
use crate::pb;
use crate::runtime::Module;

const SVC_PARSER: i32 = 0;
const MID_PARSE_STATEMENT: i32 = 10;
const MID_UNPARSE: i32 = 12;

const SVC_PARSER_OPTIONS: i32 = 699;
const MID_NEW_PARSER_OPTIONS: i32 = 0;
const MID_FREE_PARSER_OPTIONS: i32 = 12;

const SVC_PARSER_OUTPUT: i32 = 700;
const MID_PARSER_OUTPUT_NODE: i32 = 3;
const MID_FREE_PARSER_OUTPUT: i32 = 9;

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
            module.parse_with_options(sql, options.ptr())
        })
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
