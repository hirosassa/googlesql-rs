//! GoogleSQL パーサの高レベルAPI。
//!
//! 呼び出しチェーン(svc/mid は `docs/SPIKE.md` 参照):
//! NewParserOptions(699,0) → ParseStatement(0,10) → ParserOutput.Node(700,3)
//! → Unparse(0,12)。確保したハンドルは破棄する。

use crate::error::Error;
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

/// パース済みステートメント。
///
/// MVP では正規化(unparse)後のSQL文字列を保持する。将来 AST アクセサを追加予定。
#[derive(Debug, Clone)]
pub struct ParsedStatement {
    canonical_sql: String,
}

impl ParsedStatement {
    /// 正規化された標準SQL文字列。
    pub fn canonical_sql(&self) -> &str {
        &self.canonical_sql
    }
}

impl Module {
    /// SQL 文をパースし、正規化した結果を返す。
    ///
    /// 構文エラー時は [`Error::GoogleSql`] を返す。
    pub fn parse_statement(&mut self, sql: &str) -> Result<ParsedStatement, Error> {
        let options = self.invoke(SVC_PARSER_OPTIONS, MID_NEW_PARSER_OPTIONS, &[])?;
        check_error(&options)?;
        let options_ptr = pb::read_handle_at_field(&options, 1);
        if options_ptr == 0 {
            return Err(Error::GoogleSql("NewParserOptions returned null".into()));
        }

        // options は結果の成否に関わらず解放する。
        let parsed = self.parse_with_options(sql, options_ptr);
        let freed = self.invoke(
            SVC_PARSER_OPTIONS,
            MID_FREE_PARSER_OPTIONS,
            &pb::handle_arg(options_ptr),
        );

        let parsed = parsed?;
        freed?;
        Ok(parsed)
    }

    /// 構築済み ParserOptions を使ってパースし、正規化SQLまで求める。
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
            return Err(Error::GoogleSql("ParseStatement returned null".into()));
        }

        // ParserOutput は結果の成否に関わらず解放する(AST木ごと解放される)。
        let canonical = self.canonicalize_output(output_ptr);
        let freed = self.invoke(
            SVC_PARSER_OUTPUT,
            MID_FREE_PARSER_OUTPUT,
            &pb::handle_arg(output_ptr),
        );

        let canonical = canonical?;
        freed?;
        Ok(ParsedStatement {
            canonical_sql: canonical,
        })
    }

    /// ParserOutput の AST ルートを取り出し、Unparse で正規化SQLを得る。
    fn canonicalize_output(&mut self, output_ptr: u64) -> Result<String, Error> {
        let node_resp = self.invoke(
            SVC_PARSER_OUTPUT,
            MID_PARSER_OUTPUT_NODE,
            &pb::handle_arg(output_ptr),
        )?;
        check_error(&node_resp)?;
        let node_ptr = pb::read_handle_at_field(&node_resp, 1);
        if node_ptr == 0 {
            return Err(Error::GoogleSql("ParserOutput.Node returned null".into()));
        }

        let unparsed = self.invoke(SVC_PARSER, MID_UNPARSE, &pb::handle_arg(node_ptr))?;
        check_error(&unparsed)?;
        pb::read_string_at_field(&unparsed, 1)
            .ok_or_else(|| Error::GoogleSql("Unparse returned no string".into()))
    }
}

/// 応答にエラー(field 15)があれば [`Error::GoogleSql`] に変換する。
fn check_error(resp: &[u8]) -> Result<(), Error> {
    match pb::extract_error(resp) {
        Some(message) => Err(Error::GoogleSql(message)),
        None => Ok(()),
    }
}
