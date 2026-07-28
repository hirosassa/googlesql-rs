//! Represents the GoogleSQL AST as a self-contained Rust tree.
//!
//! The wasm-internal AST (arena-owned) is traversed exactly once recursively,
//! copying each node's type name, source byte range, and children into Rust.
//! The resulting [`AstNode`] holds no wasm handles, so it can be freely
//! traversed and retained after parsing.
//!
//! The ASTNodeBase (svc 331) accessors used here are documented in `docs/SPIKE.md`.

use std::ops::Range;

use crate::error::Error;
use crate::pb;
use crate::runtime::Module;

const SVC_AST_NODE_BASE: i32 = 331;
const MID_NUM_CHILDREN: i32 = 32;
const MID_CHILD: i32 = 26;
const MID_START_LOCATION: i32 = 39;
const MID_END_LOCATION: i32 = 27;

const SVC_LOCATION_POINT: i32 = 692;
const MID_GET_BYTE_OFFSET: i32 = 4;

const EXPORT_TYPE_NAME: &str = "wasmify_get_type_name";
const TYPE_NAME_PREFIX: &str = "googlesql::";

/// A single node in the AST.
#[derive(Debug, Clone)]
pub struct AstNode {
    kind: String,
    byte_range: Option<Range<usize>>,
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

        let num_children = self.node_num_children(node_ptr)?;
        let mut children = Vec::new();
        for i in 0..num_children {
            let child_ptr = self.node_child(node_ptr, i)?;
            if child_ptr != 0 {
                children.push(self.build_ast(child_ptr)?);
            }
        }

        Ok(AstNode {
            kind,
            byte_range,
            children,
        })
    }

    /// Returns the type name of the node (with the `googlesql::` prefix stripped).
    fn node_kind(&mut self, node_ptr: u64) -> Result<String, Error> {
        let mut req = Vec::new();
        pb::append_uint64(&mut req, 1, node_ptr);
        let resp = self.call_export(EXPORT_TYPE_NAME, &req)?;
        check_error(&resp)?;
        let name = pb::read_string_at_field(&resp, 1)
            .ok_or_else(|| Error::GoogleSql("type name not found".into()))?;
        Ok(name
            .strip_prefix(TYPE_NAME_PREFIX)
            .unwrap_or(&name)
            .to_owned())
    }

    /// Returns the number of children of a node.
    fn node_num_children(&mut self, node_ptr: u64) -> Result<i32, Error> {
        let resp = self.invoke(
            SVC_AST_NODE_BASE,
            MID_NUM_CHILDREN,
            &pb::handle_arg(node_ptr),
        )?;
        check_error(&resp)?;
        Ok(pb::read_int32_at_field(&resp, 1).unwrap_or(0))
    }

    /// Returns the handle of the `i`-th child node.
    fn node_child(&mut self, node_ptr: u64, i: i32) -> Result<u64, Error> {
        let mut req = Vec::new();
        pb::append_handle(&mut req, 1, node_ptr);
        pb::append_int32(&mut req, 2, i);
        let resp = self.invoke(SVC_AST_NODE_BASE, MID_CHILD, &req)?;
        check_error(&resp)?;
        Ok(pb::read_handle_at_field(&resp, 1))
    }

    /// Returns the source byte range of a node. Returns `None` if position information is unavailable.
    fn node_byte_range(&mut self, node_ptr: u64) -> Result<Option<Range<usize>>, Error> {
        let start_point = self.rpc_handle(SVC_AST_NODE_BASE, MID_START_LOCATION, node_ptr)?;
        let end_point = self.rpc_handle(SVC_AST_NODE_BASE, MID_END_LOCATION, node_ptr)?;
        if start_point == 0 || end_point == 0 {
            return Ok(None);
        }
        let start = self.point_byte_offset(start_point)?;
        let end = self.point_byte_offset(end_point)?;
        if start < 0 || end < start {
            return Ok(None);
        }
        let start = usize::try_from(start).map_err(|e| Error::GoogleSql(e.to_string()))?;
        let end = usize::try_from(end).map_err(|e| Error::GoogleSql(e.to_string()))?;
        Ok(Some(start..end))
    }

    /// Returns the byte offset of a `ParseLocationPoint`.
    fn point_byte_offset(&mut self, point_ptr: u64) -> Result<i32, Error> {
        let resp = self.invoke(
            SVC_LOCATION_POINT,
            MID_GET_BYTE_OFFSET,
            &pb::handle_arg(point_ptr),
        )?;
        check_error(&resp)?;
        Ok(pb::read_int32_at_field(&resp, 1).unwrap_or(-1))
    }

    /// Common helper: passes a single handle and returns the handle from field 1 of the response.
    fn rpc_handle(&mut self, svc: i32, mid: i32, ptr: u64) -> Result<u64, Error> {
        let resp = self.invoke(svc, mid, &pb::handle_arg(ptr))?;
        check_error(&resp)?;
        Ok(pb::read_handle_at_field(&resp, 1))
    }
}

/// Converts an error in field 15 of the response into [`Error::GoogleSql`].
fn check_error(resp: &[u8]) -> Result<(), Error> {
    pb::extract_error(resp).map_or(Ok(()), |message| Err(Error::GoogleSql(message)))
}
