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

/// Appends a handle (pointer) field in submessage form to `buf`.
pub fn append_handle(buf: &mut Vec<u8>, field: u32, ptr: u64) {
    let inner_len = varint_len(ptr).checked_add(1).unwrap_or(0);
    append_tag(buf, field, 2);
    append_varint(buf, inner_len);
    buf.push(0x08); // inner field 1, wire type 0
    append_varint(buf, ptr);
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

/// Reads an int32 (varint) at the given field number from a response.
pub fn read_int32_at_field(resp: &[u8], field: u32) -> Option<i32> {
    let mut cur = resp;
    while let Some((f, w)) = read_tag(&mut cur) {
        if f == field {
            if w == 0 {
                let v = read_varint(&mut cur)?;
                let low = u32::try_from(v & 0xFFFF_FFFF).ok()?;
                return Some(i32::from_ne_bytes(low.to_ne_bytes()));
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
    fn handle_submessage_roundtrip() {
        let mut buf = Vec::new();
        append_handle(&mut buf, 2, 0xDEAD_BEEF);
        assert_eq!(read_handle_at_field(&buf, 2), 0xDEAD_BEEF);
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
    fn error_field_is_extracted() {
        let mut buf = Vec::new();
        append_string(&mut buf, 15, "syntax error");
        assert_eq!(extract_error(&buf).as_deref(), Some("syntax error"));
        // When the error field is absent, None is returned.
        let mut ok = Vec::new();
        append_handle(&mut ok, 2, 1);
        assert_eq!(extract_error(&ok), None);
    }
}
