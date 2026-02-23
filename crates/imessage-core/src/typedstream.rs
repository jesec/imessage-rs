//! Apple typedstream decoder for chat.db attributedBody blobs.
//!
//! The `attributedBody` column in chat.db uses Apple's legacy typedstream
//! format (NSArchiver) to serialize NSMutableAttributedString objects.
//!
//! This module decodes these blobs to extract:
//! - Plain text (for universalText fallback when `text` column is null)
//! - Full structure with runs/attributes (for API `attributedBody` field)
//!
//! Supported Objective-C types:
//! - NSString / NSMutableString
//! - NSAttributedString / NSMutableAttributedString
//! - NSDictionary / NSMutableDictionary
//! - NSData / NSMutableData
//! - NSNumber / NSValue
//! - NSObject (base class, skipped)

use serde_json::{Map, Value, json};
use tracing::debug;

// --- Tag constants (from typedstream binary format) ---
const TAG_INTEGER_2: i8 = -127;
const TAG_INTEGER_4: i8 = -126;
const TAG_FLOATING_POINT: i8 = -125;
const TAG_NEW: i8 = -124;
const TAG_NIL: i8 = -123;
const TAG_END_OF_OBJECT: i8 = -122;
const FIRST_TAG: i8 = -128;
const LAST_TAG: i8 = -111;
const FIRST_REF: i64 = LAST_TAG as i64 + 1; // -110

fn in_tag_range(v: i8) -> bool {
    (FIRST_TAG..=LAST_TAG).contains(&v)
}

#[derive(Clone, Copy)]
enum ByteOrder {
    Big,
    Little,
}

/// Internal result type — we use `()` as the error since we just
/// return `None` from the public API on any decode failure.
type R<T> = Result<T, ()>;

/// A decoded class from the typedstream class chain.
#[derive(Clone, Debug)]
struct ClassInfo {
    name: String,
    #[allow(dead_code)]
    version: i64,
}

/// Internal representation of a decoded value.
#[derive(Clone, Debug)]
enum Decoded {
    Null,
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Object(Box<DecodedObject>),
}

/// Internal representation of a decoded Objective-C object.
#[derive(Clone, Debug)]
enum DecodedObject {
    String(String),
    AttributedString {
        string: String,
        runs: Vec<AttrRun>,
    },
    Dictionary(Vec<(Decoded, Decoded)>),
    Data(Vec<u8>),
    Number(Decoded),
    #[allow(dead_code)]
    Generic(String),
}

/// A single attribute run in an NSAttributedString.
#[derive(Clone, Debug)]
struct AttrRun {
    offset: usize,
    length: usize,
    attributes: Vec<(String, Decoded)>,
}

/// The main typedstream decoder.
struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    order: ByteOrder,
    /// Shared string table (low-level, used by readSharedString).
    strings: Vec<Vec<u8>>,
    /// Shared object table (high-level, used for object/class/cstring references).
    objects: Vec<Option<Decoded>>,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> R<Self> {
        let mut d = Self {
            data,
            pos: 0,
            order: ByteOrder::Big,
            strings: Vec::new(),
            objects: Vec::new(),
        };
        d.read_header()?;
        Ok(d)
    }

    fn read_header(&mut self) -> R<()> {
        let version = self.read_integer(false)?;
        if version != 4 {
            return Err(());
        }
        let sig_len = self.read_integer(false)? as usize;
        if sig_len != 11 {
            return Err(());
        }
        let sig = self.read_exact(sig_len)?;
        self.order = match sig {
            b"typedstream" => ByteOrder::Big,
            b"streamtyped" => ByteOrder::Little,
            _ => return Err(()),
        };
        let _system_version = self.read_integer(false)?;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn read_exact(&mut self, n: usize) -> R<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn read_head(&mut self) -> R<i8> {
        Ok(self.read_exact(1)?[0] as i8)
    }

    // --- Integer reading ---

    fn read_integer(&mut self, signed: bool) -> R<i64> {
        let head = self.read_head()?;
        self.read_integer_h(signed, head)
    }

    fn read_integer_h(&mut self, signed: bool, head: i8) -> R<i64> {
        if !in_tag_range(head) {
            return Ok(if signed {
                head as i64
            } else {
                (head as u8) as i64
            });
        }
        match head {
            TAG_INTEGER_2 => {
                let b = self.read_exact(2)?;
                Ok(match self.order {
                    ByteOrder::Little => {
                        if signed {
                            i16::from_le_bytes([b[0], b[1]]) as i64
                        } else {
                            u16::from_le_bytes([b[0], b[1]]) as i64
                        }
                    }
                    ByteOrder::Big => {
                        if signed {
                            i16::from_be_bytes([b[0], b[1]]) as i64
                        } else {
                            u16::from_be_bytes([b[0], b[1]]) as i64
                        }
                    }
                })
            }
            TAG_INTEGER_4 => {
                let b = self.read_exact(4)?;
                Ok(match self.order {
                    ByteOrder::Little => {
                        if signed {
                            i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64
                        } else {
                            u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64
                        }
                    }
                    ByteOrder::Big => {
                        if signed {
                            i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64
                        } else {
                            u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64
                        }
                    }
                })
            }
            _ => Err(()),
        }
    }

    // --- Float reading ---

    fn read_float_h(&mut self, head: i8) -> R<f64> {
        if head == TAG_FLOATING_POINT {
            let b = self.read_exact(4)?;
            Ok(match self.order {
                ByteOrder::Little => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                ByteOrder::Big => f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
            })
        } else {
            Ok(self.read_integer_h(true, head)? as f64)
        }
    }

    fn read_double_h(&mut self, head: i8) -> R<f64> {
        if head == TAG_FLOATING_POINT {
            let b = self.read_exact(8)?;
            Ok(match self.order {
                ByteOrder::Little => f64::from_le_bytes(b.try_into().map_err(|_| ())?),
                ByteOrder::Big => f64::from_be_bytes(b.try_into().map_err(|_| ())?),
            })
        } else {
            Ok(self.read_integer_h(true, head)? as f64)
        }
    }

    // --- String reading ---

    fn read_unshared_string_h(&mut self, head: i8) -> R<Option<Vec<u8>>> {
        if head == TAG_NIL {
            return Ok(None);
        }
        let len = self.read_integer_h(false, head)? as usize;
        Ok(Some(self.read_exact(len)?.to_vec()))
    }

    fn read_shared_string_h(&mut self, head: i8) -> R<Option<Vec<u8>>> {
        if head == TAG_NIL {
            return Ok(None);
        }
        if head == TAG_NEW {
            let h2 = self.read_head()?;
            let s = self.read_unshared_string_h(h2)?.ok_or(())?;
            self.strings.push(s.clone());
            return Ok(Some(s));
        }
        // Reference into shared string table
        let ref_num = self.read_integer_h(true, head)?;
        let idx = (ref_num - FIRST_REF) as usize;
        self.strings.get(idx).cloned().map(Some).ok_or(())
    }

    fn read_shared_string(&mut self) -> R<Option<Vec<u8>>> {
        let h = self.read_head()?;
        self.read_shared_string_h(h)
    }

    // --- Class chain reading ---

    /// Read a class chain, returning the most-specific class info.
    /// Pushes all new classes to the shared object table.
    fn read_class(&mut self) -> R<Option<ClassInfo>> {
        let mut head = self.read_head()?;
        let mut classes = Vec::new();

        while head == TAG_NEW {
            let name_bytes = self.read_shared_string()?.ok_or(())?;
            let name = String::from_utf8_lossy(&name_bytes).into_owned();
            let version = self.read_integer(true)?;
            classes.push(ClassInfo { name, version });
            head = self.read_head()?;
        }

        // Terminator: either NIL (root class) or reference to previously seen class
        let mut ref_class = None;
        if head != TAG_NIL {
            let ref_num = self.read_integer_h(true, head)?;
            let idx = (ref_num - FIRST_REF) as usize;
            // Look up the class name from the objects table
            if let Some(Some(Decoded::Str(name))) = self.objects.get(idx) {
                ref_class = Some(ClassInfo {
                    name: name.clone(),
                    version: 0,
                });
            }
        }

        // Push new classes to shared object table with their names
        for class in &classes {
            self.objects.push(Some(Decoded::Str(class.name.clone())));
        }

        // Return the most-specific class (first in chain), or the referenced class
        Ok(classes.into_iter().next().or(ref_class))
    }

    // --- Typed values reading ---

    /// Read the next typed values group from the stream.
    /// Returns the encoding string and decoded values.
    fn read_typed_values(&mut self) -> R<(String, Vec<Decoded>)> {
        let head = self.read_head()?;
        let enc_bytes = self.read_shared_string_h(head)?.ok_or(())?;
        let enc = String::from_utf8_lossy(&enc_bytes).into_owned();

        let encodings = split_encodings(&enc);
        let mut values = Vec::new();
        for encoding in &encodings {
            values.push(self.read_value(encoding)?);
        }
        Ok((enc, values))
    }

    /// Read the next typed values group and return the first value.
    fn read_typed_value(&mut self) -> R<Decoded> {
        let (_, vals) = self.read_typed_values()?;
        vals.into_iter().next().ok_or(())
    }

    // --- Value reading (dispatched by type encoding) ---

    fn read_value(&mut self, encoding: &str) -> R<Decoded> {
        match encoding {
            // Char types are always stored literally (no tags)
            "C" => Ok(Decoded::Int(self.read_exact(1)?[0] as i64)),
            "c" => Ok(Decoded::Int(self.read_exact(1)?[0] as i8 as i64)),
            // Unsigned integer types
            "S" | "I" | "L" | "Q" => {
                let h = self.read_head()?;
                Ok(Decoded::Int(self.read_integer_h(false, h)?))
            }
            // Signed integer types
            "s" | "i" | "l" | "q" => {
                let h = self.read_head()?;
                Ok(Decoded::Int(self.read_integer_h(true, h)?))
            }
            // Float
            "f" => {
                let h = self.read_head()?;
                Ok(Decoded::Float(self.read_float_h(h)?))
            }
            // Double
            "d" => {
                let h = self.read_head()?;
                Ok(Decoded::Float(self.read_double_h(h)?))
            }
            // Unshared string
            "+" => {
                let h = self.read_head()?;
                match self.read_unshared_string_h(h)? {
                    Some(b) => Ok(Decoded::Str(String::from_utf8_lossy(&b).into_owned())),
                    None => Ok(Decoded::Null),
                }
            }
            // C string (shared, with object table entry)
            "*" => {
                let h = self.read_head()?;
                self.read_cstring(h)
            }
            // Atom (shared string)
            "%" => {
                let h = self.read_head()?;
                match self.read_shared_string_h(h)? {
                    Some(b) => Ok(Decoded::Str(String::from_utf8_lossy(&b).into_owned())),
                    None => Ok(Decoded::Null),
                }
            }
            // Selector (shared string)
            ":" => {
                let h = self.read_head()?;
                match self.read_shared_string_h(h)? {
                    Some(b) => Ok(Decoded::Str(String::from_utf8_lossy(&b).into_owned())),
                    None => Ok(Decoded::Null),
                }
            }
            // Class
            "#" => match self.read_class()? {
                Some(c) => Ok(Decoded::Str(c.name)),
                None => Ok(Decoded::Null),
            },
            // Object
            "@" => self.read_object(),
            // Ignored field
            "!" => Ok(Decoded::Null),
            // Array
            enc if enc.starts_with('[') => self.read_array(enc),
            // Struct
            enc if enc.starts_with('{') => self.read_struct(enc),
            _ => Err(()),
        }
    }

    // --- C string reading (with object table) ---

    fn read_cstring(&mut self, head: i8) -> R<Decoded> {
        if head == TAG_NIL {
            return Ok(Decoded::Null);
        }
        if head == TAG_NEW {
            let s = self.read_shared_string()?.ok_or(())?;
            let val = Decoded::Str(String::from_utf8_lossy(&s).into_owned());
            self.objects.push(Some(val.clone()));
            return Ok(val);
        }
        // Object reference
        let ref_num = self.read_integer_h(true, head)?;
        let idx = (ref_num - FIRST_REF) as usize;
        self.objects.get(idx).and_then(|o| o.clone()).ok_or(())
    }

    // --- Object reading ---

    fn read_object(&mut self) -> R<Decoded> {
        let head = self.read_head()?;
        if head == TAG_NIL {
            return Ok(Decoded::Null);
        }
        if head != TAG_NEW {
            // Object reference
            let ref_num = self.read_integer_h(true, head)?;
            let idx = (ref_num - FIRST_REF) as usize;
            return self.objects.get(idx).and_then(|o| o.clone()).ok_or(());
        }

        // Literal object — reserve a placeholder in the object table
        let obj_idx = self.objects.len();
        self.objects.push(None);

        // Read class chain
        let class = self.read_class()?.ok_or(())?;

        // Dispatch to class-specific handler
        let decoded = match class.name.as_str() {
            "NSString" | "NSMutableString" => self.decode_nsstring()?,
            "NSAttributedString" | "NSMutableAttributedString" => {
                self.decode_nsattributedstring()?
            }
            "NSDictionary" | "NSMutableDictionary" => self.decode_nsdictionary()?,
            "NSData" | "NSMutableData" => self.decode_nsdata()?,
            "NSNumber" | "NSValue" => self.decode_nsnumber()?,
            "NSObject" => Decoded::Null,
            _ => self.decode_generic()?,
        };

        // Replace placeholder with decoded object
        self.objects[obj_idx] = Some(decoded.clone());

        // Read EndOfObject marker
        let end = self.read_head()?;
        if end != TAG_END_OF_OBJECT {
            return Err(());
        }

        Ok(decoded)
    }

    // --- Class-specific decoders ---

    /// NSString / NSMutableString: reads text via "+" type encoding.
    fn decode_nsstring(&mut self) -> R<Decoded> {
        let val = self.read_typed_value()?;
        match val {
            Decoded::Str(s) => Ok(Decoded::Object(Box::new(DecodedObject::String(s)))),
            _ => Err(()),
        }
    }

    /// NSAttributedString / NSMutableAttributedString.
    /// Reads: embedded NSString (the text) + attribute runs.
    fn decode_nsattributedstring(&mut self) -> R<Decoded> {
        // Read the embedded NSString via typed values "@" → NSString object
        let string_val = self.read_typed_value()?;
        let text = match &string_val {
            Decoded::Object(obj) => match obj.as_ref() {
                DecodedObject::String(s) => s.clone(),
                _ => return Err(()),
            },
            _ => return Err(()),
        };

        // Read attribute runs. NSString length is in UTF-16 code units
        // (matching JavaScript's String.length), not UTF-8 bytes.
        let text_len = text.encode_utf16().count();
        let runs = self.read_attributed_string_runs(text_len)?;

        Ok(Decoded::Object(Box::new(DecodedObject::AttributedString {
            string: text,
            runs,
        })))
    }

    /// Read attribute runs for an NSAttributedString.
    /// Each run has a (reference_index, length) pair followed by an NSDictionary
    /// of attributes (only if this reference_index hasn't been seen before).
    fn read_attributed_string_runs(&mut self, text_len: usize) -> R<Vec<AttrRun>> {
        let mut runs = Vec::new();
        let mut index = 0usize;
        let mut shared_attrs: std::collections::HashMap<i64, Vec<(String, Decoded)>> =
            std::collections::HashMap::new();

        while index < text_len {
            // Read range: typed values "is" → (reference_index, run_length)
            let (_, range_vals) = self.read_typed_values()?;
            if range_vals.len() < 2 {
                return Err(());
            }
            let reference = match &range_vals[0] {
                Decoded::Int(n) => *n,
                _ => return Err(()),
            };
            let run_length = match &range_vals[1] {
                Decoded::Int(n) => *n as usize,
                _ => return Err(()),
            };

            // If this reference hasn't been seen, read the attribute dictionary
            if let std::collections::hash_map::Entry::Vacant(e) = shared_attrs.entry(reference) {
                let dict_val = self.read_typed_value()?;
                let attrs = extract_dict_attrs(&dict_val);
                e.insert(attrs);
            }

            let attributes = shared_attrs.get(&reference).cloned().unwrap_or_default();
            runs.push(AttrRun {
                offset: index,
                length: run_length,
                attributes,
            });
            index += run_length;
        }

        Ok(runs)
    }

    /// NSDictionary / NSMutableDictionary.
    fn decode_nsdictionary(&mut self) -> R<Decoded> {
        let count_val = self.read_typed_value()?;
        let count = match count_val {
            Decoded::Int(n) => n as usize,
            _ => return Err(()),
        };

        let mut pairs = Vec::new();
        for _ in 0..count {
            let key = self.read_typed_value()?;
            let value = self.read_typed_value()?;
            pairs.push((key, value));
        }

        Ok(Decoded::Object(Box::new(DecodedObject::Dictionary(pairs))))
    }

    /// NSData / NSMutableData.
    fn decode_nsdata(&mut self) -> R<Decoded> {
        // Read length (typed values "i" → integer)
        let len_val = self.read_typed_value()?;
        let len = match len_val {
            Decoded::Int(n) => n as usize,
            _ => return Err(()),
        };

        // Read byte array (typed values "[Nc]" → byte array)
        let data_val = self.read_typed_value()?;
        let bytes = match data_val {
            Decoded::Bytes(b) => b,
            _ => {
                // Fallback: try to read `len` bytes worth of integer values
                // This shouldn't happen for standard NSData but handle gracefully
                Vec::new()
            }
        };

        // Truncate or pad to expected length
        let bytes = if bytes.len() >= len {
            bytes[..len].to_vec()
        } else {
            bytes
        };

        Ok(Decoded::Object(Box::new(DecodedObject::Data(bytes))))
    }

    /// NSNumber / NSValue: reads type encoding + value.
    fn decode_nsnumber(&mut self) -> R<Decoded> {
        // Read type encoding (typed values "*" → C string)
        let _enc_val = self.read_typed_value()?;
        // Read value with that encoding (typed values with the given encoding)
        let val = self.read_typed_value()?;
        Ok(Decoded::Object(Box::new(DecodedObject::Number(val))))
    }

    /// Generic/unknown object: skip all contents until EndOfObject.
    fn decode_generic(&mut self) -> R<Decoded> {
        loop {
            if self.eof() {
                return Err(());
            }
            let head = self.read_head()?;
            if head == TAG_END_OF_OBJECT {
                // Put back the byte — the caller reads EndOfObject
                self.pos -= 1;
                return Ok(Decoded::Null);
            }
            // Read a typed values group (encoding + values)
            let enc_bytes = self.read_shared_string_h(head)?.ok_or(())?;
            let enc = String::from_utf8_lossy(&enc_bytes).into_owned();
            let encodings = split_encodings(&enc);
            for encoding in &encodings {
                let _ = self.read_value(encoding)?;
            }
        }
    }

    // --- Array and struct reading ---

    fn read_array(&mut self, encoding: &str) -> R<Decoded> {
        let (len, elem_enc) = parse_array_encoding(encoding)?;
        if elem_enc == "c" || elem_enc == "C" {
            // Byte array — read all at once
            let data = self.read_exact(len)?.to_vec();
            return Ok(Decoded::Bytes(data));
        }
        // Non-byte array
        let mut elems = Vec::new();
        for _ in 0..len {
            elems.push(self.read_value(&elem_enc)?);
        }
        // We don't have a proper array variant, just return Null
        // (non-byte arrays are rare in chat.db)
        Ok(Decoded::Null)
    }

    fn read_struct(&mut self, encoding: &str) -> R<Decoded> {
        let field_encs = parse_struct_encoding(encoding)?;
        for enc in &field_encs {
            let _ = self.read_value(enc)?;
        }
        // Structs are not common in chat.db attributedBody
        Ok(Decoded::Null)
    }

    /// Decode all top-level typed value groups.
    fn decode_all(&mut self) -> R<Vec<Decoded>> {
        let mut results = Vec::new();
        while !self.eof() {
            match self.read_typed_values() {
                Ok((_, vals)) => results.extend(vals),
                Err(_) => break,
            }
        }
        Ok(results)
    }
}

// --- Encoding string parsing ---

/// Split a type encoding string into individual encodings.
/// e.g. "is" → ["i", "s"], "@" → ["@"], "[5c]" → ["[5c]"]
fn split_encodings(enc: &str) -> Vec<String> {
    let bytes = enc.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'[' | b'{' | b'(' => {
                // Find matching close bracket
                let open = bytes[i];
                let close = match open {
                    b'[' => b']',
                    b'{' => b'}',
                    b'(' => b')',
                    _ => unreachable!(),
                };
                let mut depth = 1;
                i += 1;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == open {
                        depth += 1;
                    } else if bytes[i] == close {
                        depth -= 1;
                    }
                    i += 1;
                }
                result.push(enc[start..i].to_string());
            }
            _ => {
                i += 1;
                result.push(enc[start..i].to_string());
            }
        }
    }
    result
}

/// Parse an array type encoding like "[5c]" → (5, "c").
fn parse_array_encoding(enc: &str) -> R<(usize, String)> {
    let inner = enc
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or(())?;
    let num_end = inner
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(inner.len());
    let len: usize = inner[..num_end].parse().map_err(|_| ())?;
    let elem = inner[num_end..].to_string();
    Ok((len, elem))
}

/// Parse a struct type encoding like "{name=ii}" → ["i", "i"].
fn parse_struct_encoding(enc: &str) -> R<Vec<String>> {
    let inner = enc
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or(())?;
    // Skip optional name before '='
    let fields_str = match inner.find('=') {
        Some(pos) => &inner[pos + 1..],
        None => inner,
    };
    Ok(split_encodings(fields_str))
}

// --- Decoded → JSON conversion ---

/// Extract key-value pairs from a decoded NSDictionary value.
fn extract_dict_attrs(val: &Decoded) -> Vec<(String, Decoded)> {
    match val {
        Decoded::Object(obj) => match obj.as_ref() {
            DecodedObject::Dictionary(pairs) => {
                let mut result = Vec::new();
                for (k, v) in pairs {
                    let key = match k {
                        Decoded::Object(ko) => match ko.as_ref() {
                            DecodedObject::String(s) => s.clone(),
                            _ => continue,
                        },
                        Decoded::Str(s) => s.clone(),
                        _ => continue,
                    };
                    result.push((key, v.clone()));
                }
                result
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Convert a Decoded value to serde_json::Value.
fn decoded_to_json(val: &Decoded) -> Value {
    match val {
        Decoded::Null => Value::Null,
        Decoded::Int(n) => json!(n),
        Decoded::Float(f) => json!(f),
        Decoded::Str(s) => json!(s),
        Decoded::Bytes(_) => Value::Null,
        Decoded::Object(obj) => match obj.as_ref() {
            DecodedObject::String(s) => json!(s),
            DecodedObject::Number(v) => decoded_to_json(v),
            DecodedObject::Data(bytes) => {
                // Try recursive decode
                if let Some(decoded) = decode_attributed_body(bytes) {
                    return decoded;
                }
                // Data that can't be decoded is skipped in decodable mode
                Value::Null
            }
            DecodedObject::AttributedString { string, runs } => {
                json!({
                    "string": string,
                    "runs": runs.iter().map(|r| {
                        let mut attrs = Map::new();
                        for (key, val) in &r.attributes {
                            let json_val = decoded_to_json(val);
                            // Skip null values from failed Data decodes
                            if !json_val.is_null() || !matches!(val, Decoded::Object(o) if matches!(o.as_ref(), DecodedObject::Data(_))) {
                                attrs.insert(key.clone(), json_val);
                            }
                        }
                        json!({
                            "range": [r.offset, r.length],
                            "attributes": attrs,
                        })
                    }).collect::<Vec<_>>()
                })
            }
            DecodedObject::Dictionary(pairs) => {
                let mut map = Map::new();
                for (k, v) in pairs {
                    let key = match k {
                        Decoded::Object(ko) => match ko.as_ref() {
                            DecodedObject::String(s) => s.clone(),
                            _ => continue,
                        },
                        Decoded::Str(s) => s.clone(),
                        _ => continue,
                    };
                    map.insert(key, decoded_to_json(v));
                }
                Value::Object(map)
            }
            DecodedObject::Generic(_) => Value::Null,
        },
    }
}

// --- Public API ---

/// Decode an attributedBody typedstream blob to JSON.
///
/// Returns an array of NSAttributedString objects, each with `string` and
/// `runs` fields.
///
/// Returns `None` if the blob is invalid or doesn't contain attributedBody data.
pub fn decode_attributed_body(data: &[u8]) -> Option<Value> {
    if data.is_empty() {
        return None;
    }

    let mut decoder = match Decoder::new(data) {
        Ok(d) => d,
        Err(_) => {
            debug!("Failed to parse typedstream header");
            return None;
        }
    };

    let values = match decoder.decode_all() {
        Ok(v) => v,
        Err(_) => {
            debug!("Failed to decode typedstream values");
            return None;
        }
    };

    // Filter for NSAttributedString instances and flatten
    let mut attributed_strings = Vec::new();
    for val in &values {
        if let Decoded::Object(obj) = val
            && let DecodedObject::AttributedString { .. } = obj.as_ref()
        {
            attributed_strings.push(decoded_to_json(val));
        }
    }

    if attributed_strings.is_empty() {
        // Flatten all values (some blobs may not be NSAttributedString)
        let all: Vec<Value> = values.iter().map(decoded_to_json).collect();
        if all.is_empty() {
            None
        } else {
            Some(Value::Array(all))
        }
    } else {
        Some(Value::Array(attributed_strings))
    }
}

/// Extract plain text from an attributedBody typedstream blob.
///
/// This is used for the `universalText()` fallback when the `text` column
/// in chat.db is null (e.g., outgoing self-messages on Tahoe/macOS 26).
///
/// Returns `None` if the blob is invalid or doesn't contain text.
pub fn extract_text(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return None;
    }

    let mut decoder = match Decoder::new(data) {
        Ok(d) => d,
        Err(_) => return None,
    };

    let values = match decoder.decode_all() {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Find the first NSAttributedString and extract its string
    for val in &values {
        if let Decoded::Object(obj) = val {
            match obj.as_ref() {
                DecodedObject::AttributedString { string, .. } => {
                    if !string.is_empty() {
                        return Some(string.clone());
                    }
                }
                DecodedObject::String(s) => {
                    if !s.is_empty() {
                        return Some(s.clone());
                    }
                }
                _ => {}
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to decode hex string to bytes.
    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decode_real_attributed_body() {
        // Real attributedBody blob from chat.db containing:
        // "Ha! The Li Ka-shing classic. Words to live by 😄"
        let hex = "040B73747265616D747970656481E803840140848484124E534174747269627574\
                   6564537472696E67008484084E534F626A656374008592848484084E5353747269\
                   6E67019484012B3248612120546865204C69204B612D7368696E6720636C617373\
                   69632E20576F72647320746F206C69766520627920F09F988486840269490130928\
                   484840C4E5344696374696F6E617279009484016901928496961D5F5F6B494D4D65\
                   7373616765506172744174747269627574654E616D65869284848408\
                   4E534E756D626572008484074E5356616C7565009484012A84999900868686";
        let data = hex_to_bytes(hex);

        // Test text extraction
        let text = extract_text(&data);
        assert_eq!(
            text,
            Some("Ha! The Li Ka-shing classic. Words to live by 😄".to_string())
        );

        // Test full decode
        let json = decode_attributed_body(&data).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["string"],
            "Ha! The Li Ka-shing classic. Words to live by 😄"
        );
        assert!(arr[0]["runs"].is_array());
        let runs = arr[0]["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1);
        // 48 UTF-16 code units (46 BMP chars + 1 surrogate pair for 😄)
        assert_eq!(runs[0]["range"], json!([0, 48]));
        assert_eq!(runs[0]["attributes"]["__kIMMessagePartAttributeName"], 0);
    }

    #[test]
    fn split_encodings_simple() {
        assert_eq!(split_encodings("@"), vec!["@"]);
        assert_eq!(split_encodings("is"), vec!["i", "s"]);
        assert_eq!(split_encodings("+"), vec!["+"]);
    }

    #[test]
    fn split_encodings_array() {
        assert_eq!(split_encodings("[5c]"), vec!["[5c]"]);
        assert_eq!(split_encodings("[10C]i"), vec!["[10C]", "i"]);
    }

    #[test]
    fn split_encodings_struct() {
        assert_eq!(split_encodings("{name=ii}"), vec!["{name=ii}"]);
    }

    #[test]
    fn parse_array_encoding_basic() {
        assert_eq!(parse_array_encoding("[5c]"), Ok((5, "c".to_string())));
        assert_eq!(parse_array_encoding("[10C]"), Ok((10, "C".to_string())));
        assert_eq!(parse_array_encoding("[100i]"), Ok((100, "i".to_string())));
    }

    #[test]
    fn parse_struct_encoding_basic() {
        assert_eq!(
            parse_struct_encoding("{name=ii}"),
            Ok(vec!["i".to_string(), "i".to_string()])
        );
        assert_eq!(
            parse_struct_encoding("{ii}"),
            Ok(vec!["i".to_string(), "i".to_string()])
        );
    }

    #[test]
    fn empty_data_returns_none() {
        assert!(extract_text(&[]).is_none());
        assert!(decode_attributed_body(&[]).is_none());
    }

    #[test]
    fn invalid_data_returns_none() {
        assert!(extract_text(b"not a typedstream").is_none());
        assert!(decode_attributed_body(b"not a typedstream").is_none());
    }

    #[test]
    fn integer_reading() {
        // Test the integer decoder with various encodings
        let mut decoder = Decoder {
            data: &[42],
            pos: 0,
            order: ByteOrder::Big,
            strings: Vec::new(),
            objects: Vec::new(),
        };
        assert_eq!(decoder.read_integer(false).unwrap(), 42);

        // Negative literal (signed)
        let mut decoder = Decoder {
            data: &[0xFE], // -2 as i8
            pos: 0,
            order: ByteOrder::Big,
            strings: Vec::new(),
            objects: Vec::new(),
        };
        assert_eq!(decoder.read_integer(true).unwrap(), -2);

        // 2-byte big-endian
        let mut decoder = Decoder {
            data: &[TAG_INTEGER_2 as u8, 0x01, 0x00], // 256 BE
            pos: 0,
            order: ByteOrder::Big,
            strings: Vec::new(),
            objects: Vec::new(),
        };
        assert_eq!(decoder.read_integer(false).unwrap(), 256);
    }

    #[test]
    fn decoded_to_json_primitives() {
        assert_eq!(decoded_to_json(&Decoded::Null), Value::Null);
        assert_eq!(decoded_to_json(&Decoded::Int(42)), json!(42));
        assert_eq!(decoded_to_json(&Decoded::Float(3.14)), json!(3.14));
        assert_eq!(
            decoded_to_json(&Decoded::Str("hello".into())),
            json!("hello")
        );
    }

    #[test]
    fn decoded_to_json_nsstring() {
        let val = Decoded::Object(Box::new(DecodedObject::String("test".into())));
        assert_eq!(decoded_to_json(&val), json!("test"));
    }

    #[test]
    fn decoded_to_json_nsnumber() {
        let val = Decoded::Object(Box::new(DecodedObject::Number(Decoded::Int(7))));
        assert_eq!(decoded_to_json(&val), json!(7));
    }

    #[test]
    fn decoded_to_json_attributed_string() {
        let val = Decoded::Object(Box::new(DecodedObject::AttributedString {
            string: "Hello".into(),
            runs: vec![AttrRun {
                offset: 0,
                length: 5,
                attributes: vec![(
                    "__kIMMessagePartAttributeName".into(),
                    Decoded::Object(Box::new(DecodedObject::Number(Decoded::Int(0)))),
                )],
            }],
        }));
        let json = decoded_to_json(&val);
        assert_eq!(json["string"], "Hello");
        assert_eq!(json["runs"][0]["range"], json!([0, 5]));
        assert_eq!(
            json["runs"][0]["attributes"]["__kIMMessagePartAttributeName"],
            0
        );
    }
}
