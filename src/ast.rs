//! Represents the GoogleSQL AST as a self-contained Rust tree.
//!
//! The wasm-internal AST (arena-owned) is traversed exactly once recursively,
//! copying each node's type name, source byte range, and children into Rust.
//! The resulting [`AstNode`] holds no wasm handles, so it can be freely
//! traversed and retained after parsing.
//!
//! The ASTNodeBase (svc 331) accessors used here are documented in `docs/SPIKE.md`.

use std::ops::Range;

use crate::error::{Error, check_error};
use crate::pb;
use crate::runtime::Module;

const SVC_AST_NODE_BASE: i32 = 331;
const MID_NUM_CHILDREN: i32 = 32;
const MID_CHILD: i32 = 26;
const MID_START_LOCATION: i32 = 39;
const MID_END_LOCATION: i32 = 27;

const SVC_LOCATION_POINT: i32 = 692;
const MID_GET_BYTE_OFFSET: i32 = 4;

const SVC_AST_IDENTIFIER: i32 = 280;
const MID_IDENTIFIER_GET_AS_STRING: i32 = 1;

const SVC_AST_BINARY_EXPRESSION: i32 = 72;
const MID_BINARY_IS_NOT: i32 = 3;
const MID_BINARY_OP: i32 = 5;

const KIND_IDENTIFIER: &str = "ASTIdentifier";
const KIND_BINARY_EXPRESSION: &str = "ASTBinaryExpression";

const EXPORT_TYPE_NAME: &str = "wasmify_get_type_name";
const TYPE_NAME_PREFIX: &str = "googlesql::";

/// The operator of an `ASTBinaryExpression`, together with whether it is negated.
///
/// Groups the operator token with the optional leading `NOT` (as in `NOT LIKE`,
/// `IS NOT`, `IS NOT DISTINCT FROM`), the way [`text`](AstNode::text) cannot: a
/// plain `a LIKE b` and `a NOT LIKE b` share the same [`operator`](Self::operator)
/// and are told apart only by [`is_negated`](Self::is_negated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryOperator {
    operator: BinaryOp,
    negated: bool,
}

impl BinaryOperator {
    /// The operator token (ignoring any leading `NOT`).
    pub const fn operator(&self) -> BinaryOp {
        self.operator
    }

    /// Whether the operator is negated by a leading `NOT` (e.g. `NOT LIKE`, `IS NOT`).
    pub const fn is_negated(&self) -> bool {
        self.negated
    }
}

/// The operator token of a [`BinaryOperator`].
///
/// Marked `#[non_exhaustive]` because GoogleSQL may add operators (its grammar
/// grows over time); adding a variant later must not break callers that match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BinaryOp {
    /// `LIKE`.
    Like,
    /// `IS` (e.g. `IS NULL`, `IS TRUE`).
    Is,
    /// `=`.
    Eq,
    /// `!=`.
    Ne,
    /// `<>`.
    Ne2,
    /// `>`.
    Gt,
    /// `<`.
    Lt,
    /// `>=`.
    Ge,
    /// `<=`.
    Le,
    /// `|`.
    BitwiseOr,
    /// `^`.
    BitwiseXor,
    /// `&`.
    BitwiseAnd,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Multiply,
    /// `/`.
    Divide,
    /// `||` (string/array concatenation).
    Concat,
    /// `IS DISTINCT FROM`.
    Distinct,
    /// `IS SOURCE OF` (graph edge direction).
    IsSourceNode,
    /// `IS DEST OF` (graph edge direction).
    IsDestNode,
}

impl BinaryOp {
    /// Maps a GoogleSQL `ASTBinaryExpression::Op` wire value to a variant.
    ///
    /// Errors on an unknown value (including the `NotSet` sentinel `1`) rather
    /// than defaulting, so a genuine binary expression never reports a wrong
    /// operator silently.
    fn from_wire(raw: i32) -> Result<Self, Error> {
        Ok(match raw {
            2 => Self::Like,
            3 => Self::Is,
            4 => Self::Eq,
            5 => Self::Ne,
            6 => Self::Ne2,
            7 => Self::Gt,
            8 => Self::Lt,
            9 => Self::Ge,
            10 => Self::Le,
            11 => Self::BitwiseOr,
            12 => Self::BitwiseXor,
            13 => Self::BitwiseAnd,
            14 => Self::Plus,
            15 => Self::Minus,
            16 => Self::Multiply,
            17 => Self::Divide,
            18 => Self::Concat,
            19 => Self::Distinct,
            20 => Self::IsSourceNode,
            21 => Self::IsDestNode,
            other => return Err(Error::Protocol(format!("unknown binary operator {other}"))),
        })
    }
}

/// A single node in the AST.
#[derive(Debug, Clone)]
pub struct AstNode {
    kind: String,
    byte_range: Option<Range<usize>>,
    identifier: Option<String>,
    binary_operator: Option<BinaryOperator>,
    children: Vec<Self>,
}

impl AstNode {
    /// The node's type name (e.g. `ASTQueryStatement`). The `googlesql::` prefix is stripped.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The byte range of this node within the original SQL. `None` if position information is unavailable.
    pub fn byte_range(&self) -> Option<Range<usize>> {
        self.byte_range.clone()
    }

    /// The child nodes.
    pub fn children(&self) -> &[Self] {
        &self.children
    }

    /// The canonical, unquoted name of an `ASTIdentifier` node (e.g. `` `my col` `` yields `my col`).
    ///
    /// `None` for every other node kind. Unlike [`text`](Self::text), this needs no
    /// original SQL and strips any backtick quoting.
    pub fn identifier(&self) -> Option<&str> {
        self.identifier.as_deref()
    }

    /// The operator of an `ASTBinaryExpression` node (e.g. `a NOT LIKE b`).
    ///
    /// `None` for every other node kind. Carries the negation flag, which the
    /// operator token alone cannot express — see [`BinaryOperator`].
    pub const fn binary_operator(&self) -> Option<BinaryOperator> {
        self.binary_operator
    }

    /// Extracts the source text for this node from the original SQL string.
    pub fn text<'a>(&self, sql: &'a str) -> Option<&'a str> {
        let range = self.byte_range.clone()?;
        sql.get(range)
    }
}

impl Module {
    /// Builds a self-contained [`AstNode`] tree from the handle of the AST root node.
    pub(crate) fn build_ast(&mut self, node_ptr: u64) -> Result<AstNode, Error> {
        let kind = self.node_kind(node_ptr)?;
        let byte_range = self.node_byte_range(node_ptr)?;
        let identifier = if kind == KIND_IDENTIFIER {
            Some(self.node_identifier(node_ptr)?)
        } else {
            None
        };
        let binary_operator = if kind == KIND_BINARY_EXPRESSION {
            Some(self.node_binary_operator(node_ptr)?)
        } else {
            None
        };

        let num_children = self.node_num_children(node_ptr)?;
        let mut children = Vec::with_capacity(usize::try_from(num_children).unwrap_or(0));
        for i in 0..num_children {
            let child_ptr = self.node_child(node_ptr, i)?;
            if child_ptr != 0 {
                children.push(self.build_ast(child_ptr)?);
            }
        }

        Ok(AstNode {
            kind,
            byte_range,
            identifier,
            binary_operator,
            children,
        })
    }

    /// Returns the canonical, unquoted name string of an `ASTIdentifier` node.
    fn node_identifier(&mut self, node_ptr: u64) -> Result<String, Error> {
        let resp =
            self.invoke_handle(SVC_AST_IDENTIFIER, MID_IDENTIFIER_GET_AS_STRING, node_ptr)?;
        check_error(&resp)?;
        pb::read_string_at_field(&resp, 1)
            .ok_or_else(|| Error::Protocol("identifier string not found".into()))
    }

    /// Returns the operator (with its negation flag) of an `ASTBinaryExpression` node.
    fn node_binary_operator(&mut self, node_ptr: u64) -> Result<BinaryOperator, Error> {
        let op_resp = self.invoke_handle(SVC_AST_BINARY_EXPRESSION, MID_BINARY_OP, node_ptr)?;
        check_error(&op_resp)?;
        let raw = pb::read_int32_at_field(&op_resp, 1)
            .ok_or_else(|| Error::Protocol("binary operator not found".into()))?;
        let operator = BinaryOp::from_wire(raw)?;

        let not_resp =
            self.invoke_handle(SVC_AST_BINARY_EXPRESSION, MID_BINARY_IS_NOT, node_ptr)?;
        check_error(&not_resp)?;
        let negated = pb::read_bool_at_field(&not_resp, 1);

        Ok(BinaryOperator { operator, negated })
    }

    /// Returns the type name of the node (with the `googlesql::` prefix stripped).
    ///
    /// Works for any handle: `wasmify_get_type_name` reports the C++ type name,
    /// so the resolved-AST walk reuses it to identify node kinds too.
    pub(crate) fn node_kind(&mut self, node_ptr: u64) -> Result<String, Error> {
        let resp =
            self.call_export_encoded(EXPORT_TYPE_NAME, |buf| pb::append_uint64(buf, 1, node_ptr))?;
        check_error(&resp)?;
        let name = pb::read_string_at_field(&resp, 1)
            .ok_or_else(|| Error::Protocol("type name not found".into()))?;
        Ok(name
            .strip_prefix(TYPE_NAME_PREFIX)
            .unwrap_or(&name)
            .to_owned())
    }

    /// Returns the number of children of a node.
    fn node_num_children(&mut self, node_ptr: u64) -> Result<i32, Error> {
        let resp = self.invoke_handle(SVC_AST_NODE_BASE, MID_NUM_CHILDREN, node_ptr)?;
        check_error(&resp)?;
        Ok(pb::read_int32_at_field(&resp, 1).unwrap_or(0))
    }

    /// Returns the handle of the `i`-th child node.
    fn node_child(&mut self, node_ptr: u64, i: i32) -> Result<u64, Error> {
        let resp = self.invoke_encoded(SVC_AST_NODE_BASE, MID_CHILD, |buf| {
            pb::append_handle(buf, 1, node_ptr);
            pb::append_int32(buf, 2, i);
        })?;
        check_error(&resp)?;
        Ok(pb::read_handle_at_field(&resp, 1))
    }

    /// Returns the source byte range of a node. Returns `None` if position information is unavailable.
    fn node_byte_range(&mut self, node_ptr: u64) -> Result<Option<Range<usize>>, Error> {
        let start_point = self.rpc_handle(SVC_AST_NODE_BASE, MID_START_LOCATION, node_ptr)?;
        let end_point = self.rpc_handle(SVC_AST_NODE_BASE, MID_END_LOCATION, node_ptr)?;
        self.byte_range_from_points(start_point, end_point)
    }

    /// Converts a pair of `ParseLocationPoint` handles into a source byte range.
    ///
    /// Returns `None` when either point is null or the offsets do not form a
    /// valid forward range (a negative offset or `end` before `start`), so
    /// callers can treat "no usable location" uniformly. Shared by the parser
    /// AST and the resolved AST, which obtain the two points differently.
    pub(crate) fn byte_range_from_points(
        &mut self,
        start_point: u64,
        end_point: u64,
    ) -> Result<Option<Range<usize>>, Error> {
        if start_point == 0 || end_point == 0 {
            return Ok(None);
        }
        let start = self.point_byte_offset(start_point)?;
        let end = self.point_byte_offset(end_point)?;
        if start < 0 || end < start {
            return Ok(None);
        }
        let start = usize::try_from(start).map_err(|e| Error::Protocol(e.to_string()))?;
        let end = usize::try_from(end).map_err(|e| Error::Protocol(e.to_string()))?;
        Ok(Some(start..end))
    }

    /// Returns the byte offset of a `ParseLocationPoint`.
    fn point_byte_offset(&mut self, point_ptr: u64) -> Result<i32, Error> {
        let resp = self.invoke_handle(SVC_LOCATION_POINT, MID_GET_BYTE_OFFSET, point_ptr)?;
        check_error(&resp)?;
        Ok(pb::read_int32_at_field(&resp, 1).unwrap_or(-1))
    }

    /// Common helper: passes a single handle and returns the handle from field 1 of the response.
    pub(crate) fn rpc_handle(&mut self, svc: i32, mid: i32, ptr: u64) -> Result<u64, Error> {
        let resp = self.invoke_handle(svc, mid, ptr)?;
        check_error(&resp)?;
        Ok(pb::read_handle_at_field(&resp, 1))
    }
}
