//! Minimal NBT writer. Only the tags the Sponge schematic format needs.
//!
//! NBT is big-endian, tag-prefixed, with names as modified-UTF8 length-prefixed
//! strings. We only ever *write*, so a builder-style tree plus a serializer is
//! enough; there is no parser here.

#[derive(Debug, Clone)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    ByteArray(Vec<u8>),
    String(String),
    List(u8, Vec<Tag>),
    Compound(Vec<(String, Tag)>),
    IntArray(Vec<i32>),
}

impl Tag {
    fn id(&self) -> u8 {
        match self {
            Tag::Byte(_) => 1,
            Tag::Short(_) => 2,
            Tag::Int(_) => 3,
            Tag::Long(_) => 4,
            Tag::ByteArray(_) => 7,
            Tag::String(_) => 8,
            Tag::List(..) => 9,
            Tag::Compound(_) => 10,
            Tag::IntArray(_) => 11,
        }
    }

    fn write_payload(&self, out: &mut Vec<u8>) {
        match self {
            Tag::Byte(v) => out.push(*v as u8),
            Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::ByteArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                out.extend_from_slice(v);
            }
            Tag::String(s) => write_string(out, s),
            Tag::List(elem_id, items) => {
                // An empty list is conventionally written with element type TAG_End.
                let id = if items.is_empty() { 0 } else { *elem_id };
                out.push(id);
                out.extend_from_slice(&(items.len() as i32).to_be_bytes());
                for it in items {
                    it.write_payload(out);
                }
            }
            Tag::Compound(fields) => {
                for (name, tag) in fields {
                    out.push(tag.id());
                    write_string(out, name);
                    tag.write_payload(out);
                }
                out.push(0); // TAG_End
            }
            Tag::IntArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for i in v {
                    out.extend_from_slice(&i.to_be_bytes());
                }
            }
        }
    }
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    // Modified UTF-8 differs from UTF-8 only for NUL and supplementary-plane
    // characters. Block identifiers and our metadata are plain ASCII, so a
    // debug assert is a cheaper guard than a full encoder we would never exercise.
    debug_assert!(s.is_ascii(), "NBT writer only handles ASCII names: {s:?}");
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Serialize a named root tag to uncompressed NBT bytes.
pub fn write_root(name: &str, tag: &Tag) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(tag.id());
    write_string(&mut out, name);
    tag.write_payload(&mut out);
    out
}

/// LEB128-style varint, as used by the schematic block data array.
pub fn write_varint(out: &mut Vec<u8>, mut value: u32) {
    loop {
        if value & !0x7f == 0 {
            out.push(value as u8);
            return;
        }
        out.push(((value & 0x7f) | 0x80) as u8);
        value >>= 7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_matches_leb128() {
        let cases: [(u32, &[u8]); 5] = [
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (300, &[0xac, 0x02]),
        ];
        for (v, expect) in cases {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            assert_eq!(buf, expect, "varint({v})");
        }
    }

    #[test]
    fn compound_roundtrip_shape() {
        let t = Tag::Compound(vec![("A".into(), Tag::Int(1))]);
        let bytes = write_root("R", &t);
        // 0a 0001 'R' | 03 0001 'A' 00000001 | 00
        assert_eq!(
            bytes,
            vec![0x0a, 0x00, 0x01, b'R', 0x03, 0x00, 0x01, b'A', 0, 0, 0, 1, 0x00]
        );
    }

    #[test]
    fn empty_list_uses_tag_end() {
        let mut buf = Vec::new();
        Tag::List(10, vec![]).write_payload(&mut buf);
        assert_eq!(buf, vec![0x00, 0, 0, 0, 0]);
    }
}
