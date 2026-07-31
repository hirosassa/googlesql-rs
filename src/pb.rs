//! Minimal protobuf wire-format encode/decode used by wasmify.
//!
//! See `docs/SPIKE.md` for the specification. Handles (object pointers) use the
//! submessage form (`tag(f,2) + len + 0x08 + varint(ptr)`); constructor responses
//! store the pointer as a bare varint.

// ---- encode ----------------------------------------------------------------

/// Appends a varint (LEB128) to `buf`.
pub fn append_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let low = u8::try_from(v & 0x7f).unwrap_or(0);
        let next = v.checked_shr(7).unwrap_or(0);
        if next == 0 {
            buf.push(low);
            return;
        }
        buf.push(low | 0x80);
        v = next;
    }
}

/// Appends a field tag (`field << 3 | wire`) to `buf`.
pub fn append_tag(buf: &mut Vec<u8>, field: u32, wire: u32) {
    let tag = u64::from(field).checked_shl(3).unwrap_or(0) | u64::from(wire);
    append_varint(buf, tag);
}

/// Appends a string field (wire type 2) to `buf`.
pub fn append_string(buf: &mut Vec<u8>, field: u32, s: &str) {
    let bytes = s.as_bytes();
    append_tag(buf, field, 2);
    append_varint(buf, u64::try_from(bytes.len()).unwrap_or(0));
    buf.extend_from_slice(bytes);
}

/// Appends a uint64 field (wire type 0, varint) to `buf`.
pub fn append_uint64(buf: &mut Vec<u8>, field: u32, v: u64) {
    append_tag(buf, field, 0);
    append_varint(buf, v);
}

/// Appends a non-negative int32 field (wire type 0, varint) to `buf`.
pub fn append_int32(buf: &mut Vec<u8>, field: u32, v: i32) {
    append_tag(buf, field, 0);
    append_varint(buf, u64::try_from(v).unwrap_or(0));
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
    let inner_len = varint_len(ptr).checked_add(1).unwrap_or(0);
    append_tag(buf, field, 2);
    append_varint(buf, inner_len);
    buf.push(0x08); // inner field 1, wire type 0
    append_varint(buf, ptr);
}

/// Appends a length-delimited submessage field (wire type 2) to `buf`.
///
/// `inner` is the already-encoded body of the nested message.
pub fn append_submessage(buf: &mut Vec<u8>, field: u32, inner: &[u8]) {
    append_tag(buf, field, 2);
    append_varint(buf, u64::try_from(inner.len()).unwrap_or(0));
    buf.extend_from_slice(inner);
}

/// Builds a request that contains only a single handle (field 1 = handle).
pub fn handle_arg(ptr: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    append_handle(&mut buf, 1, ptr);
    buf
}

/// Returns the number of bytes needed to encode `v` as a varint.
fn varint_len(mut v: u64) -> u64 {
    let mut n: u64 = 1;
    while v >= 0x80 {
        v = v.checked_shr(7).unwrap_or(0);
        n = n.checked_add(1).unwrap_or(n);
    }
    n
}

// ---- decode ----------------------------------------------------------------

/// Reads a varint from the cursor.
fn read_varint(cur: &mut &[u8]) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        let (&b, rest) = cur.split_first()?;
        *cur = rest;
        let part = u64::from(b & 0x7f).checked_shl(shift)?;
        result |= part;
        if b & 0x80 == 0 {
            return Some(result);
        }
        shift = shift.checked_add(7)?;
        if shift >= 64 {
            return None;
        }
    }
}

/// Reads a field tag `(field, wire)` from the cursor.
fn read_tag(cur: &mut &[u8]) -> Option<(u32, u32)> {
    let tag = read_varint(cur)?;
    let field = u32::try_from(tag.checked_shr(3)?).ok()?;
    let wire = u32::try_from(tag & 0x7).ok()?;
    Some((field, wire))
}

/// Reads a length-prefixed (wire type 2) payload and advances the cursor.
fn read_len_prefixed<'a>(cur: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = usize::try_from(read_varint(cur)?).ok()?;
    let (head, rest) = cur.split_at_checked(len)?;
    *cur = rest;
    Some(head)
}

/// Skips a field value of the given wire type.
fn skip(cur: &mut &[u8], wire: u32) -> Option<()> {
    match wire {
        0 => read_varint(cur).map(|_| ()),
        1 => cur.split_at_checked(8).map(|(_, rest)| *cur = rest),
        2 => read_len_prefixed(cur).map(|_| ()),
        5 => cur.split_at_checked(4).map(|(_, rest)| *cur = rest),
        _ => None,
    }
}

/// Extracts the error string (field 15) from a response. Returns `None` if absent.
pub fn extract_error(resp: &[u8]) -> Option<String> {
    read_string_at_field(resp, 15)
}

/// Reads the raw bytes at the given field number from a response.
///
/// Unlike [`read_string_at_field`], this preserves the bytes verbatim rather
/// than lossily decoding them as UTF-8, so it suits `BYTES` values that may not
/// be valid UTF-8.
pub fn read_bytes_at_field(resp: &[u8], field: u32) -> Option<Vec<u8>> {
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field {
            if w == 2 {
                return read_len_prefixed(&mut cur).map(<[u8]>::to_vec);
            }
            return None;
        }
        skip(&mut cur, w)?;
    }
    None
}

/// Reads the string at the given field number from a response.
pub fn read_string_at_field(resp: &[u8], field: u32) -> Option<String> {
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field {
            if w == 2 {
                let bytes = read_len_prefixed(&mut cur)?;
                return Some(String::from_utf8_lossy(bytes).into_owned());
            }
            return None;
        }
        skip(&mut cur, w)?;
    }
    None
}

/// Reinterprets a varint's low 32 bits as an `i32` (proto `int32` decoding), so
/// a negative value serialized as its 64-bit two's-complement pattern reads back
/// correctly.
fn varint_as_i32(v: u64) -> i32 {
    let low = u32::try_from(v & 0xFFFF_FFFF).unwrap_or(0);
    i32::from_ne_bytes(low.to_ne_bytes())
}

/// Reads an int32 (varint) at the given field number from a response.
pub fn read_int32_at_field(resp: &[u8], field: u32) -> Option<i32> {
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field {
            if w == 0 {
                return read_varint(&mut cur).map(varint_as_i32);
            }
            return None;
        }
        skip(&mut cur, w)?;
    }
    None
}

/// Reads every int32 stored at the given repeated field number from a response.
///
/// GoogleSQL emits repeated `int32` either packed (a single length-delimited run
/// of varints) or unpacked (one wire-type-0 field per element); this accepts
/// both, so `column_index_list`-style accessors decode into one value per
/// element, in order. An absent field yields an empty vector.
pub fn read_int32s_at_field(resp: &[u8], field: u32) -> Vec<i32> {
    let mut out = Vec::new();
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field && w == 2 {
            let Some(mut sub) = read_len_prefixed(&mut cur) else {
                break;
            };
            while let Some(v) = read_varint(&mut sub) {
                out.push(varint_as_i32(v));
            }
        } else if f == field && w == 0 {
            let Some(v) = read_varint(&mut cur) else {
                break;
            };
            out.push(varint_as_i32(v));
        } else if skip(&mut cur, w).is_none() {
            break;
        }
    }
    out
}

/// Reads a bool (varint) at the given field number from a response.
///
/// Returns `false` when the field is absent, matching proto3 scalar defaults
/// (a `false` value is not serialized).
pub fn read_bool_at_field(resp: &[u8], field: u32) -> bool {
    read_int32_at_field(resp, field).is_some_and(|v| v != 0)
}

/// Reads an int64 (varint) at the given field number from a response.
///
/// The wire value is a two's-complement varint (proto `int64`), so a negative
/// literal comes back as the full 64-bit pattern.
pub fn read_int64_at_field(resp: &[u8], field: u32) -> Option<i64> {
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field {
            if w == 0 {
                let v = read_varint(&mut cur)?;
                return Some(i64::from_ne_bytes(v.to_ne_bytes()));
            }
            return None;
        }
        skip(&mut cur, w)?;
    }
    None
}

/// Reads a double (fixed64) at the given field number from a response.
///
/// A proto `double` is a wire-type-1 field: eight little-endian IEEE-754 bytes.
pub fn read_double_at_field(resp: &[u8], field: u32) -> Option<f64> {
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field {
            if w == 1 {
                let bytes: [u8; 8] = cur.get(..8)?.try_into().ok()?;
                return Some(f64::from_le_bytes(bytes));
            }
            return None;
        }
        skip(&mut cur, w)?;
    }
    None
}

/// Reads a handle (pointer) at the given field number from a response. Returns `0` if absent.
pub fn read_handle_at_field(resp: &[u8], field: u32) -> u64 {
    if field == 1 {
        return read_handle_ptr(resp);
    }
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field {
            if w == 2
                && let Some(sub) = read_len_prefixed(&mut cur)
            {
                return read_handle_ptr(sub);
            }
            return 0;
        }
        if skip(&mut cur, w).is_none() {
            break;
        }
    }
    0
}

/// Reads every handle stored at the given repeated field number from a response.
///
/// Each occurrence is a length-delimited submessage carrying the handle (the
/// same encoding [`append_handle`] produces), so `output_column_list`-style
/// repeated accessors decode into one handle per element, in order.
pub fn read_handles_at_field(resp: &[u8], field: u32) -> Vec<u64> {
    let mut out = Vec::new();
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field && w == 2 {
            let Some(sub) = read_len_prefixed(&mut cur) else {
                break;
            };
            out.push(read_handle_ptr(sub));
        } else if skip(&mut cur, w).is_none() {
            break;
        }
    }
    out
}

/// Reads a handle pointer, supporting both bare-varint and submessage forms.
fn read_handle_ptr(data: &[u8]) -> u64 {
    let mut cur = data;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == 1 {
            if w == 0 {
                return read_varint(&mut cur).unwrap_or(0);
            }
            if w == 2
                && let Some(mut sub) = read_len_prefixed(&mut cur)
            {
                while let Some((sf, sw)) = read_tag(&mut sub) {
                    if sf == 1 && sw == 0 {
                        return read_varint(&mut sub).unwrap_or(0);
                    }
                    if skip(&mut sub, sw).is_none() {
                        break;
                    }
                }
            }
            return 0;
        }
        if skip(&mut cur, w).is_none() {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let mut buf = Vec::new();
            append_varint(&mut buf, v);
            let mut cur = buf.as_slice();
            assert_eq!(read_varint(&mut cur), Some(v));
            assert!(cur.is_empty());
        }
    }

    #[test]
    fn string_field_roundtrip() {
        let mut buf = Vec::new();
        append_string(&mut buf, 1, "SELECT 1");
        assert_eq!(read_string_at_field(&buf, 1).as_deref(), Some("SELECT 1"));
    }

    #[test]
    fn bool_field_reads_true_false_and_default() {
        // An explicit `true` reads back as true.
        let mut t = Vec::new();
        append_bool(&mut t, 1, true);
        assert!(read_bool_at_field(&t, 1));

        // An explicit `false` (varint 0) reads back as false.
        let mut f = Vec::new();
        append_bool(&mut f, 1, false);
        assert!(!read_bool_at_field(&f, 1));

        // An absent field defaults to false (proto3 does not serialize `false`).
        assert!(!read_bool_at_field(&[], 1));
    }

    #[test]
    fn handle_submessage_roundtrip() {
        let mut buf = Vec::new();
        append_handle(&mut buf, 2, 0xDEAD_BEEF);
        assert_eq!(read_handle_at_field(&buf, 2), 0xDEAD_BEEF);
    }

    #[test]
    fn submessage_wraps_inner_handle() {
        // A handle nested inside a submessage field must round-trip: encode the
        // inner buffer (field 4 = handle), wrap it as field 2, and read it back.
        let mut inner = Vec::new();
        append_handle(&mut inner, 4, 0xCAFE);
        let mut outer = Vec::new();
        append_submessage(&mut outer, 2, &inner);

        // The outer field 2 is a submessage; its nested field-4 handle round-trips.
        let mut cur = outer.as_slice();
        assert_eq!(read_tag(&mut cur), Some((2, 2)));
        let sub = read_len_prefixed(&mut cur);
        assert_eq!(sub.map(|s| read_handle_at_field(s, 4)), Some(0xCAFE));
    }

    #[test]
    fn repeated_handles_decode_in_order() {
        // A repeated handle field encodes as several occurrences of the same
        // field number, each a submessage carrying one handle.
        let mut buf = Vec::new();
        append_handle(&mut buf, 1, 0xAAAA);
        append_handle(&mut buf, 1, 0xBBBB);
        append_handle(&mut buf, 1, 0xCCCC);
        assert_eq!(read_handles_at_field(&buf, 1), vec![0xAAAA, 0xBBBB, 0xCCCC]);

        // An absent field yields no handles, and unrelated fields are skipped.
        let mut other = Vec::new();
        append_string(&mut other, 2, "ignored");
        assert!(read_handles_at_field(&other, 1).is_empty());

        // A same-numbered field with a different wire type is skipped, not
        // treated as a handle, and the trailing handle is still collected.
        let mut mixed = Vec::new();
        append_handle(&mut mixed, 1, 0x1111);
        append_uint64(&mut mixed, 1, 999); // field 1, wire type 0
        append_handle(&mut mixed, 1, 0x2222);
        assert_eq!(read_handles_at_field(&mixed, 1), vec![0x1111, 0x2222]);
    }

    #[test]
    fn direct_varint_handle_is_read() {
        // Constructor response form: field 1 = bare varint.
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 0);
        append_varint(&mut buf, 42);
        assert_eq!(read_handle_at_field(&buf, 1), 42);
    }

    #[test]
    fn int32_field_roundtrip() {
        for v in [0i32, 1, 127, 128, 1000, i32::MAX, -1] {
            let mut buf = Vec::new();
            append_int32(&mut buf, 3, v.max(0));
            assert_eq!(read_int32_at_field(&buf, 3), Some(v.max(0)));
        }
        // The int32 representation of -1 (lower 32 bits) must also be readable.
        let mut neg = Vec::new();
        append_uint64(&mut neg, 1, u64::from(u32::MAX));
        assert_eq!(read_int32_at_field(&neg, 1), Some(-1));
    }

    #[test]
    fn repeated_int32s_decode_packed_and_unpacked() {
        // Packed encoding: a single length-delimited run of varints.
        let mut inner = Vec::new();
        append_varint(&mut inner, 0);
        append_varint(&mut inner, 1);
        append_varint(&mut inner, 300);
        let mut packed = Vec::new();
        append_submessage(&mut packed, 1, &inner);
        assert_eq!(read_int32s_at_field(&packed, 1), vec![0, 1, 300]);

        // Non-packed encoding: one wire-type-0 field per element.
        let mut unpacked = Vec::new();
        append_int32(&mut unpacked, 1, 7);
        append_int32(&mut unpacked, 1, 8);
        assert_eq!(read_int32s_at_field(&unpacked, 1), vec![7, 8]);

        // An absent field yields no values.
        assert!(read_int32s_at_field(&[], 1).is_empty());
    }

    #[test]
    fn int64_field_reads_positive_and_negative() {
        // A positive int64 round-trips through the varint encoding.
        let mut pos = Vec::new();
        append_uint64(&mut pos, 1, 42);
        assert_eq!(read_int64_at_field(&pos, 1), Some(42));

        // A negative int64 is a full 64-bit two's-complement varint.
        let mut neg = Vec::new();
        append_uint64(&mut neg, 1, u64::from_ne_bytes((-7i64).to_ne_bytes()));
        assert_eq!(read_int64_at_field(&neg, 1), Some(-7));

        // An absent field yields None.
        assert_eq!(read_int64_at_field(&[], 1), None);
    }

    #[test]
    fn double_field_reads_ieee754_le() {
        // A double is written as a wire-type-1 field: eight little-endian bytes.
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 1);
        buf.extend_from_slice(&(2.5f64).to_le_bytes());
        assert_eq!(read_double_at_field(&buf, 1), Some(2.5));

        // A field written with a different wire type is not read as a double.
        let mut wrong = Vec::new();
        append_uint64(&mut wrong, 1, 5);
        assert_eq!(read_double_at_field(&wrong, 1), None);

        // An absent field yields None.
        assert_eq!(read_double_at_field(&[], 1), None);
    }

    #[test]
    fn bool_field_encodes_as_varint() {
        let mut t = Vec::new();
        append_bool(&mut t, 4, true);
        assert_eq!(read_int32_at_field(&t, 4), Some(1));

        let mut f = Vec::new();
        append_bool(&mut f, 4, false);
        assert_eq!(read_int32_at_field(&f, 4), Some(0));
    }

    #[test]
    fn error_field_is_extracted() {
        let mut buf = Vec::new();
        append_string(&mut buf, 15, "syntax error");
        assert_eq!(extract_error(&buf).as_deref(), Some("syntax error"));
        // When the error field is absent, None is returned.
        let mut ok = Vec::new();
        append_handle(&mut ok, 2, 1);
        assert_eq!(extract_error(&ok), None);
    }

    #[test]
    fn read_varint_rejects_truncated_continuation() {
        // A byte with the continuation bit set but no following byte is
        // malformed: the reader must stop rather than read past the end.
        assert_eq!(read_varint(&mut &[0x80][..]), None);
        assert_eq!(read_varint(&mut &[0xFF, 0xFF][..]), None);
    }

    #[test]
    fn read_varint_rejects_overflowing_shift() {
        // Ten continuation bytes push the shift past 64 bits; a well-formed
        // 64-bit varint never needs that, so this must be rejected.
        assert_eq!(read_varint(&mut &[0xFF; 10][..]), None);

        // The genuine 10-byte encoding of u64::MAX (nine 0xFF then 0x01) is the
        // boundary that *must* still decode — the guard rejects one bit more.
        let mut max = Vec::new();
        append_varint(&mut max, u64::MAX);
        assert_eq!(max.len(), 10);
        assert_eq!(read_varint(&mut max.as_slice()), Some(u64::MAX));
    }

    #[test]
    fn skip_rejects_unknown_and_group_wire_types() {
        // Wire types 3 and 4 (deprecated groups) and any value >= 6 are not
        // supported; skipping one must fail so the reader bails out.
        for wire in [3u32, 4, 6, 7] {
            assert_eq!(skip(&mut &[1, 2, 3, 4, 5, 6, 7, 8][..], wire), None);
        }
    }

    #[test]
    fn skip_rejects_truncated_fixed_width_fields() {
        // A fixed64 (wire 1) needs 8 bytes and a fixed32 (wire 5) needs 4; fewer
        // than that is truncated input and must not be skipped.
        assert_eq!(skip(&mut &[0, 0, 0][..], 1), None);
        assert_eq!(skip(&mut &[0, 0, 0][..], 5), None);
    }

    #[test]
    fn read_string_rejects_length_prefix_past_end() {
        // A length prefix claiming more bytes than remain is truncated input;
        // the reader must return None rather than read out of bounds.
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 2);
        append_varint(&mut buf, 10); // claims 10 bytes...
        buf.extend_from_slice(b"abc"); // ...but only 3 follow
        assert_eq!(read_string_at_field(&buf, 1), None);
    }

    #[test]
    fn read_string_ignores_field_with_wrong_wire_type() {
        // The requested field exists but is not length-delimited, so it is not a
        // string: reading it yields None rather than a garbled value.
        let mut buf = Vec::new();
        append_uint64(&mut buf, 1, 42);
        assert_eq!(read_string_at_field(&buf, 1), None);
    }

    #[test]
    fn int32_decodes_two_complement_boundaries() {
        // proto int32 negatives arrive as the low 32 bits of a 64-bit varint.
        // The extremes must reinterpret as the correct signed values.
        for (low, expected) in [
            (0x8000_0000u64, i32::MIN),
            (0x7FFF_FFFF, i32::MAX),
            (u64::from(u32::MAX), -1),
        ] {
            let mut buf = Vec::new();
            append_uint64(&mut buf, 1, low);
            assert_eq!(read_int32_at_field(&buf, 1), Some(expected));
        }
    }
}
