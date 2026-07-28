//! wasmify が用いる protobuf wire-format の最小 encode/decode。
//!
//! 仕様は `docs/SPIKE.md` を参照。ハンドル(オブジェクトポインタ)は
//! submessage 形(`tag(f,2) + len + 0x08 + varint(ptr)`)、コンストラクタ応答は
//! 直接 varint 形で格納される。

// ---- encode ----------------------------------------------------------------

/// varint(LEB128)を追記する。
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

/// フィールドタグ(`field << 3 | wire`)を追記する。
pub fn append_tag(buf: &mut Vec<u8>, field: u32, wire: u32) {
    let tag = u64::from(field).checked_shl(3).unwrap_or(0) | u64::from(wire);
    append_varint(buf, tag);
}

/// 文字列フィールド(wire type 2)を追記する。
pub fn append_string(buf: &mut Vec<u8>, field: u32, s: &str) {
    let bytes = s.as_bytes();
    append_tag(buf, field, 2);
    append_varint(buf, u64::try_from(bytes.len()).unwrap_or(0));
    buf.extend_from_slice(bytes);
}

/// ハンドル(ポインタ)フィールドを submessage 形で追記する。
pub fn append_handle(buf: &mut Vec<u8>, field: u32, ptr: u64) {
    let inner_len = varint_len(ptr).checked_add(1).unwrap_or(0);
    append_tag(buf, field, 2);
    append_varint(buf, inner_len);
    buf.push(0x08); // inner field 1, wire type 0
    append_varint(buf, ptr);
}

/// ハンドルだけを持つリクエスト(field 1 = ハンドル)を組み立てる。
pub fn handle_arg(ptr: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    append_handle(&mut buf, 1, ptr);
    buf
}

/// varint として符号化したときのバイト数を返す。
fn varint_len(mut v: u64) -> u64 {
    let mut n: u64 = 1;
    while v >= 0x80 {
        v = v.checked_shr(7).unwrap_or(0);
        n = n.checked_add(1).unwrap_or(n);
    }
    n
}

// ---- decode ----------------------------------------------------------------

/// カーソルから varint を読む。
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

/// カーソルからフィールドタグ `(field, wire)` を読む。
fn read_tag(cur: &mut &[u8]) -> Option<(u32, u32)> {
    let tag = read_varint(cur)?;
    let field = u32::try_from(tag.checked_shr(3)?).ok()?;
    let wire = u32::try_from(tag & 0x7).ok()?;
    Some((field, wire))
}

/// 長さ前置(wire type 2)のペイロードを読み、カーソルを進める。
fn read_len_prefixed<'a>(cur: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = usize::try_from(read_varint(cur)?).ok()?;
    let (head, rest) = cur.split_at_checked(len)?;
    *cur = rest;
    Some(head)
}

/// 指定 wire type のフィールド値を読み飛ばす。
fn skip(cur: &mut &[u8], wire: u32) -> Option<()> {
    match wire {
        0 => read_varint(cur).map(|_| ()),
        1 => cur.split_at_checked(8).map(|(_, rest)| *cur = rest),
        2 => read_len_prefixed(cur).map(|_| ()),
        5 => cur.split_at_checked(4).map(|(_, rest)| *cur = rest),
        _ => None,
    }
}

/// 応答からエラー(field 15 の string)を取り出す。無ければ `None`。
pub fn extract_error(resp: &[u8]) -> Option<String> {
    read_string_at_field(resp, 15)
}

/// 応答の指定フィールドから string を読む。
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

/// 応答の指定フィールドからハンドル(ポインタ)を読む。無ければ `0`。
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

/// 直接 varint 形と submessage 形の両方に対応してハンドルを読む。
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
        // コンストラクタ応答形: field 1 = 直接 varint。
        let mut buf = Vec::new();
        append_tag(&mut buf, 1, 0);
        append_varint(&mut buf, 42);
        assert_eq!(read_handle_at_field(&buf, 1), 42);
    }

    #[test]
    fn error_field_is_extracted() {
        let mut buf = Vec::new();
        append_string(&mut buf, 15, "syntax error");
        assert_eq!(extract_error(&buf).as_deref(), Some("syntax error"));
        // エラーフィールドが無ければ None。
        let mut ok = Vec::new();
        append_handle(&mut ok, 2, 1);
        assert_eq!(extract_error(&ok), None);
    }
}
