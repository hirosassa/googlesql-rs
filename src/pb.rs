//! Minimal protobuf wire-format encode/decode used by wasmify.
//!
//! See `docs/SPIKE.md` for the specification. Handles (object pointers) use the
//! submessage form (`tag(f,2) + len + 0x08 + varint(ptr)`); constructor responses
//! store the pointer as a bare varint.
//!
//! Decoders distinguish a field that is *absent* (`Ok(None)`, or the proto3
//! default for the infallible-by-default readers) from a payload that is
//! *malformed* (`Err(DecodeError)`): only absence may default.

use crate::error::Error;

// ---- errors ----------------------------------------------------------------

/// A malformed or truncated protobuf payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The buffer ended in the middle of a varint.
    #[error("truncated varint")]
    TruncatedVarint,
    /// A varint does not fit in 64 bits.
    #[error("varint overflows 64 bits")]
    VarintOverflow,
    /// A field tag whose field number does not fit in 32 bits.
    #[error("invalid field tag")]
    InvalidTag,
    /// A length prefix that exceeds the remaining bytes (or `usize`).
    #[error("length-delimited field is truncated")]
    TruncatedLengthDelimited,
    /// A fixed-width field with fewer bytes than its width.
    #[error("truncated fixed-width field")]
    TruncatedFixed,
    /// A wire type this decoder cannot skip (groups or reserved values).
    #[error("unsupported wire type {0}")]
    UnsupportedWireType(u32),
    /// The requested field is present with a different wire type than expected.
    #[error("field {field} has wire type {actual}, expected {expected}")]
    WireTypeMismatch {
        field: u32,
        expected: u32,
        actual: u32,
    },
}

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Self::Protocol(format!("malformed protobuf response: {e}"))
    }
}

// ---- encode ----------------------------------------------------------------

/// Appends a varint (LEB128) to `buf`.
pub fn append_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        // Low byte of the little-endian form; masked to 7 bits below.
        let [low, ..] = v.to_le_bytes();
        let low = low & 0x7f;
        v >>= 7;
        if v == 0 {
            buf.push(low);
            return;
        }
        buf.push(low | 0x80);
    }
}

/// Appends a field tag (`field << 3 | wire`) to `buf`.
pub fn append_tag(buf: &mut Vec<u8>, field: u32, wire: u32) {
    // A 32-bit field number shifted by 3 always fits in 64 bits.
    let tag = (u64::from(field) << 3) | u64::from(wire);
    append_varint(buf, tag);
}

/// Widens a slice length to `u64`.
const fn len_as_u64(len: usize) -> u64 {
    const { assert!(usize::BITS <= u64::BITS) };
    #[expect(
        clippy::as_conversions,
        reason = "usize is at most 64 bits (asserted above); there is no infallible From<usize> for u64"
    )]
    {
        len as u64
    }
}

/// Appends a string field (wire type 2) to `buf`.
pub fn append_string(buf: &mut Vec<u8>, field: u32, s: &str) {
    append_submessage(buf, field, s.as_bytes());
}

/// Appends a uint64 field (wire type 0, varint) to `buf`.
pub fn append_uint64(buf: &mut Vec<u8>, field: u32, v: u64) {
    append_tag(buf, field, 0);
    append_varint(buf, v);
}

/// Appends an int32 field (wire type 0, varint) to `buf`.
///
/// Negative values are sign-extended to 64 bits and use the 10-byte varint
/// encoding, matching protobuf's `int32`.
pub fn append_int32(buf: &mut Vec<u8>, field: u32, v: i32) {
    append_int64(buf, field, i64::from(v));
}

/// Appends an int64 field (wire type 0, varint) to `buf`.
///
/// Negative values use the full 10-byte two's-complement varint encoding, matching
/// protobuf's `int64`.
pub fn append_int64(buf: &mut Vec<u8>, field: u32, v: i64) {
    append_tag(buf, field, 0);
    append_varint(buf, u64::from_ne_bytes(v.to_ne_bytes()));
}

/// Appends a double field (wire type 1, fixed 64-bit little-endian) to `buf`.
pub fn append_double(buf: &mut Vec<u8>, field: u32, v: f64) {
    append_tag(buf, field, 1);
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Appends a bool field (wire type 0, varint `0`/`1`) to `buf`.
pub fn append_bool(buf: &mut Vec<u8>, field: u32, v: bool) {
    append_tag(buf, field, 0);
    append_varint(buf, u64::from(v));
}

/// Appends a handle (pointer) field in submessage form to `buf`.
pub fn append_handle(buf: &mut Vec<u8>, field: u32, ptr: u64) {
    let mut inner = vec![0x08]; // inner field 1, wire type 0
    append_varint(&mut inner, ptr);
    append_submessage(buf, field, &inner);
}

/// Appends a length-delimited submessage field (wire type 2) to `buf`.
///
/// `inner` is the already-encoded body of the nested message.
pub fn append_submessage(buf: &mut Vec<u8>, field: u32, inner: &[u8]) {
    append_tag(buf, field, 2);
    append_varint(buf, len_as_u64(inner.len()));
    buf.extend_from_slice(inner);
}

/// Builds a request that contains only a single handle (field 1 = handle).
pub fn handle_arg(ptr: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    append_handle(&mut buf, 1, ptr);
    buf
}

// ---- decode ----------------------------------------------------------------

type Result<T> = std::result::Result<T, DecodeError>;

/// Reads a varint from the cursor.
fn read_varint(cur: &mut &[u8]) -> Result<u64> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        let (&b, rest) = cur.split_first().ok_or(DecodeError::TruncatedVarint)?;
        *cur = rest;
        // The tenth byte only has room for bit 63; any higher payload bit would be
        // silently truncated by the shift.
        if shift == 63 && b & 0x7e != 0 {
            return Err(DecodeError::VarintOverflow);
        }
        let part = u64::from(b & 0x7f)
            .checked_shl(shift)
            .ok_or(DecodeError::VarintOverflow)?;
        result |= part;
        if b & 0x80 == 0 {
            return Ok(result);
        }
        shift = shift.checked_add(7).ok_or(DecodeError::VarintOverflow)?;
        if shift >= 64 {
            return Err(DecodeError::VarintOverflow);
        }
    }
}

/// Reads the next field tag `(field, wire)`; `Ok(None)` only at a clean end of input.
fn next_tag(cur: &mut &[u8]) -> Result<Option<(u32, u32)>> {
    if cur.is_empty() {
        return Ok(None);
    }
    let tag = read_varint(cur)?;
    let field = u32::try_from(tag >> 3).map_err(|_| DecodeError::InvalidTag)?;
    let wire = u32::try_from(tag & 0x7).map_err(|_| DecodeError::InvalidTag)?;
    Ok(Some((field, wire)))
}

/// Reads a length-prefixed (wire type 2) payload and advances the cursor.
fn read_len_prefixed<'a>(cur: &mut &'a [u8]) -> Result<&'a [u8]> {
    let len =
        usize::try_from(read_varint(cur)?).map_err(|_| DecodeError::TruncatedLengthDelimited)?;
    let (head, rest) = cur
        .split_at_checked(len)
        .ok_or(DecodeError::TruncatedLengthDelimited)?;
    *cur = rest;
    Ok(head)
}

/// Skips a field value of the given wire type.
fn skip(cur: &mut &[u8], wire: u32) -> Result<()> {
    match wire {
        0 => read_varint(cur).map(|_| ()),
        1 => skip_fixed(cur, 8),
        2 => read_len_prefixed(cur).map(|_| ()),
        5 => skip_fixed(cur, 4),
        other => Err(DecodeError::UnsupportedWireType(other)),
    }
}

fn skip_fixed(cur: &mut &[u8], width: usize) -> Result<()> {
    let (_, rest) = cur
        .split_at_checked(width)
        .ok_or(DecodeError::TruncatedFixed)?;
    *cur = rest;
    Ok(())
}

/// Finds `field` and returns the bytes starting at its value.
///
/// `Ok(None)` means the field is absent. A field present with a wire type other
/// than `wire` is malformed and reported as an error.
fn find_field(resp: &[u8], field: u32, wire: u32) -> Result<Option<&[u8]>> {
    let mut cur = resp;
    while let Some((f, w)) = next_tag(&mut cur)? {
        if f == field {
            if w == wire {
                return Ok(Some(cur));
            }
            return Err(DecodeError::WireTypeMismatch {
                field,
                expected: wire,
                actual: w,
            });
        }
        skip(&mut cur, w)?;
    }
    Ok(None)
}

/// Extracts the error string (field 15) from a response. `Ok(None)` if absent.
pub fn extract_error(resp: &[u8]) -> Result<Option<String>> {
    read_string_at_field(resp, 15)
}

/// Reads the raw bytes at the given field number from a response.
///
/// Unlike [`read_string_at_field`], this preserves the bytes verbatim rather
/// than lossily decoding them as UTF-8, so it suits `BYTES` values that may not
/// be valid UTF-8. `Ok(None)` if the field is absent.
pub fn read_bytes_at_field(resp: &[u8], field: u32) -> Result<Option<Vec<u8>>> {
    find_field(resp, field, 2)?
        .map(|mut cur| read_len_prefixed(&mut cur).map(<[u8]>::to_vec))
        .transpose()
}

/// Reads the string at the given field number from a response. `Ok(None)` if absent.
pub fn read_string_at_field(resp: &[u8], field: u32) -> Result<Option<String>> {
    Ok(read_bytes_at_field(resp, field)?.map(|b| String::from_utf8_lossy(&b).into_owned()))
}

/// The low 32 bits of a varint (proto `uint32`/`int32` decoding truncates to 32 bits).
const fn low_u32(v: u64) -> u32 {
    let [b0, b1, b2, b3, ..] = v.to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}

/// Reinterprets a varint's low 32 bits as an `i32` (proto `int32` decoding), so
/// a negative value serialized as its 64-bit two's-complement pattern reads back
/// correctly.
const fn varint_as_i32(v: u64) -> i32 {
    i32::from_ne_bytes(low_u32(v).to_ne_bytes())
}

/// Reads a varint-valued field. `Ok(None)` if absent.
fn read_varint_field(resp: &[u8], field: u32) -> Result<Option<u64>> {
    find_field(resp, field, 0)?
        .map(|mut cur| read_varint(&mut cur))
        .transpose()
}

/// Reads an int32 (varint) at the given field number from a response.
pub fn read_int32_at_field(resp: &[u8], field: u32) -> Result<Option<i32>> {
    Ok(read_varint_field(resp, field)?.map(varint_as_i32))
}

/// Reads every int32 stored at the given repeated field number from a response.
///
/// `GoogleSQL` emits repeated `int32` either packed (a single length-delimited run
/// of varints) or unpacked (one wire-type-0 field per element); this accepts
/// both, so `column_index_list`-style accessors decode into one value per
/// element, in order. An absent field yields an empty vector; a malformed
/// payload is an error.
pub fn read_int32s_at_field(resp: &[u8], field: u32) -> Result<Vec<i32>> {
    let mut out = Vec::new();
    let mut cur = resp;
    while let Some((f, w)) = next_tag(&mut cur)? {
        if f == field && w == 2 {
            let mut sub = read_len_prefixed(&mut cur)?;
            while !sub.is_empty() {
                out.push(varint_as_i32(read_varint(&mut sub)?));
            }
        } else if f == field && w == 0 {
            out.push(varint_as_i32(read_varint(&mut cur)?));
        } else {
            skip(&mut cur, w)?;
        }
    }
    Ok(out)
}

/// Reads a bool (varint) at the given field number from a response.
///
/// Returns `false` when the field is absent, matching proto3 scalar defaults
/// (a `false` value is not serialized); a malformed payload is an error.
pub fn read_bool_at_field(resp: &[u8], field: u32) -> Result<bool> {
    Ok(read_varint_field(resp, field)?.is_some_and(|v| low_u32(v) != 0))
}

/// Reads an int64 (varint) at the given field number from a response.
///
/// The wire value is a two's-complement varint (proto `int64`), so a negative
/// literal comes back as the full 64-bit pattern.
pub fn read_int64_at_field(resp: &[u8], field: u32) -> Result<Option<i64>> {
    Ok(read_varint_field(resp, field)?.map(|v| i64::from_ne_bytes(v.to_ne_bytes())))
}

/// Reads a uint32 (varint) at the given field number from a response.
pub fn read_uint32_at_field(resp: &[u8], field: u32) -> Result<Option<u32>> {
    Ok(read_varint_field(resp, field)?.map(low_u32))
}

/// Reads a uint64 (varint) at the given field number from a response.
pub fn read_uint64_at_field(resp: &[u8], field: u32) -> Result<Option<u64>> {
    read_varint_field(resp, field)
}

/// Reads a float (fixed32) at the given field number from a response.
///
/// A proto `float` is a wire-type-5 field: four little-endian IEEE-754 bytes.
pub fn read_float_at_field(resp: &[u8], field: u32) -> Result<Option<f32>> {
    find_field(resp, field, 5)?
        .map(|cur| {
            let bytes = cur.first_chunk::<4>().ok_or(DecodeError::TruncatedFixed)?;
            Ok(f32::from_le_bytes(*bytes))
        })
        .transpose()
}

/// Reads a double (fixed64) at the given field number from a response.
///
/// A proto `double` is a wire-type-1 field: eight little-endian IEEE-754 bytes.
pub fn read_double_at_field(resp: &[u8], field: u32) -> Result<Option<f64>> {
    find_field(resp, field, 1)?
        .map(|cur| {
            let bytes = cur.first_chunk::<8>().ok_or(DecodeError::TruncatedFixed)?;
            Ok(f64::from_le_bytes(*bytes))
        })
        .transpose()
}

/// Reads a handle (pointer) at the given field number from a response.
///
/// Returns `0` (the null-handle sentinel) only when the handle is absent; a
/// truncated or malformed handle is an error, never `0`.
pub fn read_handle_at_field(resp: &[u8], field: u32) -> Result<u64> {
    if field == 1 {
        return read_handle_ptr(resp);
    }
    match find_field(resp, field, 2)? {
        Some(mut cur) => read_handle_ptr(read_len_prefixed(&mut cur)?),
        None => Ok(0),
    }
}

/// Reads every handle stored at the given repeated field number from a response.
///
/// Each occurrence is a length-delimited submessage carrying the handle (the
/// same encoding [`append_handle`] produces), so `output_column_list`-style
/// repeated accessors decode into one handle per element, in order.
pub fn read_handles_at_field(resp: &[u8], field: u32) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    let mut cur = resp;
    while let Some((f, w)) = next_tag(&mut cur)? {
        if f == field && w == 2 {
            out.push(read_handle_ptr(read_len_prefixed(&mut cur)?)?);
        } else {
            skip(&mut cur, w)?;
        }
    }
    Ok(out)
}

/// Reads a handle pointer, supporting both bare-varint and submessage forms.
/// Returns `0` only when no pointer is present.
fn read_handle_ptr(data: &[u8]) -> Result<u64> {
    let mut cur = data;
    while let Some((f, w)) = next_tag(&mut cur)? {
        if f == 1 {
            return match w {
                0 => read_varint(&mut cur),
                2 => {
                    let mut sub = read_len_prefixed(&mut cur)?;
                    while let Some((sf, sw)) = next_tag(&mut sub)? {
                        if sf == 1 && sw == 0 {
                            return read_varint(&mut sub);
                        }
                        skip(&mut sub, sw)?;
                    }
                    Ok(0)
                }
                actual => Err(DecodeError::WireTypeMismatch {
                    field: 1,
                    expected: 0,
                    actual,
                }),
            };
        }
        skip(&mut cur, w)?;
    }
    Ok(0)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn varint_bytes(v: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        append_varint(&mut buf, v);
        buf
    }

    #[test]
    fn varint_roundtrip() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            16_384,
            u64::from(u32::MAX),
            u64::MAX,
        ] {
            let buf = varint_bytes(v);
            let mut cur = buf.as_slice();
            assert_eq!(read_varint(&mut cur), Ok(v));
            assert_eq!(cur, <&[u8]>::default());
        }
    }

    #[test]
    fn string_field_roundtrip() {
        let mut buf = Vec::new();
        append_string(&mut buf, 1, "SELECT 1");
        assert_eq!(
            read_string_at_field(&buf, 1).unwrap().as_deref(),
            Some("SELECT 1")
        );
    }

    #[test]
    fn bool_field_reads_true_false_and_default() {
        let mut t = Vec::new();
        append_bool(&mut t, 1, true);
        assert_eq!(read_bool_at_field(&t, 1), Ok(true));

        let mut f = Vec::new();
        append_bool(&mut f, 1, false);
        assert_eq!(read_bool_at_field(&f, 1), Ok(false));

        // An absent field defaults to false (proto3 does not serialize `false`).
        assert_eq!(read_bool_at_field(&[], 1), Ok(false));
    }

    #[test]
    fn handle_submessage_roundtrip() {
        let mut buf = Vec::new();
        append_handle(&mut buf, 2, 0xDEAD_BEEF);
        assert_eq!(read_handle_at_field(&buf, 2), Ok(0xDEAD_BEEF));
    }

    #[test]
    fn submessage_wraps_inner_handle() {
        let mut inner = Vec::new();
        append_handle(&mut inner, 4, 0xCAFE);
        let mut outer = Vec::new();
        append_submessage(&mut outer, 2, &inner);

        let mut cur = outer.as_slice();
        assert_eq!(next_tag(&mut cur), Ok(Some((2, 2))));
        let sub = read_len_prefixed(&mut cur).unwrap();
        assert_eq!(read_handle_at_field(sub, 4), Ok(0xCAFE));
    }

    #[test]
    fn repeated_handles_decode_in_order() {
        let mut buf = Vec::new();
        append_handle(&mut buf, 1, 0xAAAA);
        append_handle(&mut buf, 1, 0xBBBB);
        append_handle(&mut buf, 1, 0xCCCC);
        assert_eq!(
            read_handles_at_field(&buf, 1),
            Ok(vec![0xAAAA, 0xBBBB, 0xCCCC])
        );

        // An absent field yields no handles, and unrelated fields are skipped.
        let mut other = Vec::new();
        append_string(&mut other, 2, "ignored");
        assert_eq!(read_handles_at_field(&other, 1), Ok(Vec::new()));

        // A same-numbered field with a different wire type is skipped, not
        // treated as a handle, and the trailing handle is still collected.
        let mut mixed = Vec::new();
        append_handle(&mut mixed, 1, 0x1111);
        append_uint64(&mut mixed, 1, 999);
        append_handle(&mut mixed, 1, 0x2222);
        assert_eq!(read_handles_at_field(&mixed, 1), Ok(vec![0x1111, 0x2222]));
    }

    #[test]
    fn direct_varint_handle_is_read() {
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 0);
        append_varint(&mut buf, 42);
        assert_eq!(read_handle_at_field(&buf, 1), Ok(42));
    }

    #[test]
    fn absent_handle_is_the_null_sentinel() {
        assert_eq!(read_handle_at_field(&[], 1), Ok(0));
        assert_eq!(read_handle_at_field(&[], 2), Ok(0));
        let mut other = Vec::new();
        append_string(&mut other, 3, "x");
        assert_eq!(read_handle_at_field(&other, 2), Ok(0));
        // Submessage present but carrying no pointer is also "absent".
        let mut empty = Vec::new();
        append_submessage(&mut empty, 2, &[]);
        assert_eq!(read_handle_at_field(&empty, 2), Ok(0));
    }

    #[test]
    fn truncated_bare_handle_varint_is_an_error_not_null() {
        // field 1, wire 0, then a varint whose continuation bit has no next byte.
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 0);
        buf.push(0x80);
        assert_eq!(
            read_handle_at_field(&buf, 1),
            Err(DecodeError::TruncatedVarint)
        );
    }

    #[test]
    fn truncated_submessage_handle_varint_is_an_error_not_null() {
        // Well-formed outer frame, but the inner handle varint is cut short.
        let inner = [0x08, 0xFF];
        let mut buf = Vec::new();
        append_submessage(&mut buf, 2, &inner);
        assert_eq!(
            read_handle_at_field(&buf, 2),
            Err(DecodeError::TruncatedVarint)
        );
        assert_eq!(
            read_handles_at_field(&buf, 2),
            Err(DecodeError::TruncatedVarint)
        );
    }

    #[test]
    fn handle_with_wrong_wire_type_is_an_error() {
        // field 1 as fixed32 is neither a bare varint nor a submessage.
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 5);
        buf.extend_from_slice(&[0; 4]);
        assert_eq!(
            read_handle_at_field(&buf, 1),
            Err(DecodeError::WireTypeMismatch {
                field: 1,
                expected: 0,
                actual: 5
            })
        );
    }

    #[test]
    fn handle_submessage_truncated_length_is_an_error() {
        let mut buf = Vec::new();
        append_tag(&mut buf, 2, 2);
        append_varint(&mut buf, 9);
        buf.push(0x08);
        assert_eq!(
            read_handle_at_field(&buf, 2),
            Err(DecodeError::TruncatedLengthDelimited)
        );
    }

    #[test]
    fn int32_field_roundtrip() {
        for v in [0i32, 1, 127, 128, 1000, i32::MAX, -1, i32::MIN] {
            let mut buf = Vec::new();
            append_int32(&mut buf, 3, v);
            assert_eq!(read_int32_at_field(&buf, 3), Ok(Some(v)));
        }
        // The int32 representation of -1 (lower 32 bits) must also be readable.
        let mut neg = Vec::new();
        append_uint64(&mut neg, 1, u64::from(u32::MAX));
        assert_eq!(read_int32_at_field(&neg, 1), Ok(Some(-1)));
    }

    #[test]
    fn absent_scalar_fields_are_ok_none() {
        assert_eq!(read_int32_at_field(&[], 1), Ok(None));
        assert_eq!(read_int64_at_field(&[], 1), Ok(None));
        assert_eq!(read_uint32_at_field(&[], 1), Ok(None));
        assert_eq!(read_uint64_at_field(&[], 1), Ok(None));
        assert_eq!(read_float_at_field(&[], 1), Ok(None));
        assert_eq!(read_double_at_field(&[], 1), Ok(None));
        assert_eq!(read_string_at_field(&[], 1), Ok(None));
        assert_eq!(read_bytes_at_field(&[], 1), Ok(None));
        assert_eq!(extract_error(&[]), Ok(None));
        // An unrelated field present does not make the requested one malformed.
        let mut other = Vec::new();
        append_int32(&mut other, 2, 5);
        assert_eq!(read_int32_at_field(&other, 1), Ok(None));
    }

    #[test]
    fn truncated_int32_varint_is_an_error() {
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 0);
        buf.push(0x80);
        assert_eq!(
            read_int32_at_field(&buf, 1),
            Err(DecodeError::TruncatedVarint)
        );
        assert_eq!(
            read_bool_at_field(&buf, 1),
            Err(DecodeError::TruncatedVarint)
        );
        assert_eq!(
            read_int64_at_field(&buf, 1),
            Err(DecodeError::TruncatedVarint)
        );
        assert_eq!(
            read_uint32_at_field(&buf, 1),
            Err(DecodeError::TruncatedVarint)
        );
        assert_eq!(
            read_uint64_at_field(&buf, 1),
            Err(DecodeError::TruncatedVarint)
        );
    }

    #[test]
    fn malformed_data_before_the_requested_field_is_an_error() {
        // A truncated unrelated field precedes the wanted one: the reader cannot
        // trust anything after it, so this is malformed rather than "absent".
        let mut buf = Vec::new();
        append_tag(&mut buf, 9, 1);
        buf.extend_from_slice(&[0; 3]); // fixed64 needs 8
        append_int32(&mut buf, 1, 7);
        assert_eq!(
            read_int32_at_field(&buf, 1),
            Err(DecodeError::TruncatedFixed)
        );
    }

    #[test]
    fn int32_with_wrong_wire_type_is_an_error() {
        let mut buf = Vec::new();
        append_string(&mut buf, 1, "oops");
        assert_eq!(
            read_int32_at_field(&buf, 1),
            Err(DecodeError::WireTypeMismatch {
                field: 1,
                expected: 0,
                actual: 2
            })
        );
    }

    #[test]
    fn repeated_int32s_decode_packed_and_unpacked() {
        let mut inner = Vec::new();
        append_varint(&mut inner, 0);
        append_varint(&mut inner, 1);
        append_varint(&mut inner, 300);
        let mut packed = Vec::new();
        append_submessage(&mut packed, 1, &inner);
        assert_eq!(read_int32s_at_field(&packed, 1), Ok(vec![0, 1, 300]));

        let mut unpacked = Vec::new();
        append_int32(&mut unpacked, 1, 7);
        append_int32(&mut unpacked, 1, 8);
        assert_eq!(read_int32s_at_field(&unpacked, 1), Ok(vec![7, 8]));

        assert_eq!(read_int32s_at_field(&[], 1), Ok(Vec::new()));
    }

    #[test]
    fn repeated_int32s_reject_malformed_payloads() {
        // Packed run whose final varint is cut short.
        let mut packed = Vec::new();
        append_submessage(&mut packed, 1, &[0x01, 0x80]);
        assert_eq!(
            read_int32s_at_field(&packed, 1),
            Err(DecodeError::TruncatedVarint)
        );

        // Length prefix beyond the buffer.
        let mut cut = Vec::new();
        append_tag(&mut cut, 1, 2);
        append_varint(&mut cut, 5);
        cut.push(0x01);
        assert_eq!(
            read_int32s_at_field(&cut, 1),
            Err(DecodeError::TruncatedLengthDelimited)
        );

        // Unpacked element with a truncated varint.
        let mut unpacked = Vec::new();
        append_tag(&mut unpacked, 1, 0);
        unpacked.push(0x80);
        assert_eq!(
            read_int32s_at_field(&unpacked, 1),
            Err(DecodeError::TruncatedVarint)
        );
    }

    #[test]
    fn int64_field_reads_positive_and_negative() {
        let mut pos = Vec::new();
        append_uint64(&mut pos, 1, 42);
        assert_eq!(read_int64_at_field(&pos, 1), Ok(Some(42)));

        let mut neg = Vec::new();
        append_uint64(&mut neg, 1, u64::from_ne_bytes((-7i64).to_ne_bytes()));
        assert_eq!(read_int64_at_field(&neg, 1), Ok(Some(-7)));

        assert_eq!(read_int64_at_field(&[], 1), Ok(None));
    }

    #[test]
    fn double_field_reads_ieee754_le() {
        let mut buf = Vec::new();
        append_double(&mut buf, 1, 2.5);
        assert_eq!(read_double_at_field(&buf, 1), Ok(Some(2.5)));

        // Present with a different wire type is malformed, not a default.
        let mut wrong = Vec::new();
        append_uint64(&mut wrong, 1, 5);
        assert!(matches!(
            read_double_at_field(&wrong, 1),
            Err(DecodeError::WireTypeMismatch { .. })
        ));
    }

    #[test]
    fn truncated_fixed_width_value_is_an_error() {
        let mut dbl = Vec::new();
        append_tag(&mut dbl, 1, 1);
        dbl.extend_from_slice(&[0; 7]);
        assert_eq!(
            read_double_at_field(&dbl, 1),
            Err(DecodeError::TruncatedFixed)
        );

        let mut flt = Vec::new();
        append_tag(&mut flt, 1, 5);
        flt.extend_from_slice(&[0; 3]);
        assert_eq!(
            read_float_at_field(&flt, 1),
            Err(DecodeError::TruncatedFixed)
        );
    }

    #[test]
    fn bool_field_encodes_as_varint() {
        let mut t = Vec::new();
        append_bool(&mut t, 4, true);
        assert_eq!(read_int32_at_field(&t, 4), Ok(Some(1)));

        let mut f = Vec::new();
        append_bool(&mut f, 4, false);
        assert_eq!(read_int32_at_field(&f, 4), Ok(Some(0)));
    }

    #[test]
    fn error_field_is_extracted() {
        let mut buf = Vec::new();
        append_string(&mut buf, 15, "syntax error");
        assert_eq!(
            extract_error(&buf).unwrap().as_deref(),
            Some("syntax error")
        );
        let mut ok = Vec::new();
        append_handle(&mut ok, 2, 1);
        assert_eq!(extract_error(&ok), Ok(None));
    }

    #[test]
    fn read_varint_rejects_truncated_continuation() {
        assert_eq!(
            read_varint(&mut &[0x80][..]),
            Err(DecodeError::TruncatedVarint)
        );
        assert_eq!(
            read_varint(&mut &[0xFF, 0xFF][..]),
            Err(DecodeError::TruncatedVarint)
        );
    }

    #[test]
    fn read_varint_rejects_overflowing_shift() {
        assert_eq!(
            read_varint(&mut &[0xFF; 10][..]),
            Err(DecodeError::VarintOverflow)
        );

        // The genuine 10-byte encoding of u64::MAX is the boundary that must decode.
        let max = varint_bytes(u64::MAX);
        assert_eq!(max.len(), 10);
        assert_eq!(read_varint(&mut max.as_slice()), Ok(u64::MAX));
    }

    #[test]
    fn read_varint_rejects_truncated_tenth_byte() {
        let mut bytes = [0xFFu8; 10];
        bytes[9] = 0x7F;
        assert_eq!(
            read_varint(&mut &bytes[..]),
            Err(DecodeError::VarintOverflow)
        );
        bytes[9] = 0x02;
        assert_eq!(
            read_varint(&mut &bytes[..]),
            Err(DecodeError::VarintOverflow)
        );
        bytes[9] = 0x01;
        assert_eq!(read_varint(&mut &bytes[..]), Ok(u64::MAX));
    }

    #[test]
    fn append_int32_sign_extends_negative_values() {
        let mut buf = Vec::new();
        append_int32(&mut buf, 1, -1);
        assert_eq!(buf.len(), 1 + 10);
        assert_eq!(read_int32_at_field(&buf, 1), Ok(Some(-1)));
    }

    #[test]
    fn append_tag_encodes_large_field_numbers() {
        let mut buf = Vec::new();
        append_tag(&mut buf, u32::MAX, 2);
        let mut cur = buf.as_slice();
        assert_eq!(next_tag(&mut cur), Ok(Some((u32::MAX, 2))));
    }

    #[test]
    fn skip_rejects_unknown_and_group_wire_types() {
        for wire in [3u32, 4, 6, 7] {
            assert_eq!(
                skip(&mut &[1, 2, 3, 4, 5, 6, 7, 8][..], wire),
                Err(DecodeError::UnsupportedWireType(wire))
            );
        }
    }

    #[test]
    fn skip_rejects_truncated_fixed_width_fields() {
        assert_eq!(
            skip(&mut &[0, 0, 0][..], 1),
            Err(DecodeError::TruncatedFixed)
        );
        assert_eq!(
            skip(&mut &[0, 0, 0][..], 5),
            Err(DecodeError::TruncatedFixed)
        );
    }

    #[test]
    fn truncated_length_delimited_field_is_an_error() {
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 2);
        append_varint(&mut buf, 10); // claims 10 bytes...
        buf.extend_from_slice(b"abc"); // ...but only 3 follow
        let expected = DecodeError::TruncatedLengthDelimited;
        assert_eq!(read_string_at_field(&buf, 1), Err(expected));
        assert_eq!(read_bytes_at_field(&buf, 1), Err(expected));
    }

    #[test]
    fn string_with_wrong_wire_type_is_an_error() {
        let mut buf = Vec::new();
        append_uint64(&mut buf, 1, 42);
        assert_eq!(
            read_string_at_field(&buf, 1),
            Err(DecodeError::WireTypeMismatch {
                field: 1,
                expected: 2,
                actual: 0
            })
        );
    }

    #[test]
    fn uint32_and_uint64_decode_their_full_range() {
        let mut buf = Vec::new();
        append_uint64(&mut buf, 1, u64::from(u32::MAX));
        assert_eq!(read_uint32_at_field(&buf, 1), Ok(Some(u32::MAX)));

        let mut wide = Vec::new();
        append_uint64(&mut wide, 1, u64::MAX);
        assert_eq!(read_uint64_at_field(&wide, 1), Ok(Some(u64::MAX)));
    }

    #[test]
    fn float_decodes_a_fixed32_field() {
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 5);
        buf.extend_from_slice(&1.5f32.to_le_bytes());
        assert_eq!(read_float_at_field(&buf, 1), Ok(Some(1.5)));

        let mut wrong = Vec::new();
        append_uint64(&mut wrong, 1, 42);
        assert!(read_float_at_field(&wrong, 1).is_err());
    }

    #[test]
    fn int32_decodes_two_complement_boundaries() {
        for (low, expected) in [
            (0x8000_0000u64, i32::MIN),
            (0x7FFF_FFFF, i32::MAX),
            (u64::from(u32::MAX), -1),
        ] {
            let mut buf = Vec::new();
            append_uint64(&mut buf, 1, low);
            assert_eq!(read_int32_at_field(&buf, 1), Ok(Some(expected)));
        }
    }

    #[test]
    fn decode_error_maps_to_protocol_error() {
        let err: Error = DecodeError::TruncatedVarint.into();
        assert!(matches!(err, Error::Protocol(ref m) if m.contains("truncated varint")));
    }
}
