//! Swift `Codable` binary codec as used by the UTM Remote protocol.
//!
//! The UTM Remote protocol serialises every request/reply with Apple's
//! `PropertyListEncoder`-like binary `Codable` representation.  The format was
//! reverse engineered by pyutmremote (`utmremote/cod.py`); this module is a
//! byte-compatible reimplementation.
//!
//! Layout of a payload:
//!
//! ```text
//! uleb128 shape_count
//! repeat shape_count times:
//!     uleb128 key_count
//!     repeat key_count times:
//!         uleb128 key_len, key_bytes (UTF-8)
//! <value>
//! ```
//!
//! A keyed container (dictionary / struct / enum) stores a metadata byte, a
//! shape id, optional null bitmap, the encoded length of each non-null value,
//! then the values.  An unkeyed container (array / bytes) stores a metadata
//! byte, element count, either a fixed element size or per-element sizes, then
//! the elements.

use std::collections::{BTreeMap, HashMap};

use crate::error::{Error, Result};

/// A dynamically typed Codable value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Dict(BTreeMap<String, Value>),
}

impl Value {
    pub fn dict<I, K>(items: I) -> Value
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        let mut map = BTreeMap::new();
        for (k, v) in items {
            map.insert(k.into(), v);
        }
        Value::Dict(map)
    }

    pub fn empty() -> Value {
        Value::Dict(BTreeMap::new())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

fn push_uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| Error::Codable("unexpected end of input".into()))?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(Error::Codable("unexpected end of input".into()));
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn uleb(&mut self) -> Result<u64> {
        let mut value: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            if shift >= 64 {
                return Err(Error::Codable("ULEB128 overflow".into()));
            }
            value |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    #[allow(dead_code)]
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    #[allow(dead_code)]
    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

struct ShapeTable {
    shapes: Vec<Vec<String>>,
}

impl ShapeTable {
    fn parse(reader: &mut Reader<'_>) -> Result<Self> {
        let count = reader.uleb()? as usize;
        if count > 4096 {
            return Err(Error::Codable("unreasonable shape count".into()));
        }
        let mut shapes = Vec::with_capacity(count);
        for _ in 0..count {
            let key_count = reader.uleb()? as usize;
            if key_count > 4096 {
                return Err(Error::Codable("unreasonable key count".into()));
            }
            let mut keys = Vec::with_capacity(key_count);
            for _ in 0..key_count {
                let len = reader.uleb()? as usize;
                let bytes = reader.take(len)?;
                keys.push(
                    std::str::from_utf8(bytes)
                        .map_err(|_| Error::Codable("shape key is not UTF-8".into()))?
                        .to_owned(),
                );
            }
            shapes.push(keys);
        }
        Ok(Self { shapes })
    }

    fn get(&self, id: u64) -> Result<&[String]> {
        self.shapes
            .get(id as usize)
            .map(|s| s.as_slice())
            .ok_or_else(|| Error::Codable(format!("shape id {id} out of range")))
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

struct Encoder {
    shapes: Vec<Vec<String>>,
    shape_ids: HashMap<Vec<String>, u64>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            shapes: Vec::new(),
            shape_ids: HashMap::new(),
        }
    }

    fn shape_id(&mut self, keys: &[String]) -> u64 {
        if let Some(id) = self.shape_ids.get(keys) {
            return *id;
        }
        let id = self.shapes.len() as u64;
        let owned = keys.to_vec();
        self.shapes.push(owned.clone());
        self.shape_ids.insert(owned, id);
        id
    }

    fn encode(&mut self, value: &Value, out: &mut Vec<u8>) -> Result<()> {
        match value {
            Value::Null => Err(Error::Codable("cannot encode bare null".into())),
            Value::Bool(b) => {
                out.push(if *b { 1 } else { 0 });
                Ok(())
            }
            Value::Int(i) => {
                out.extend_from_slice(&i.to_le_bytes());
                Ok(())
            }
            Value::Str(s) => {
                push_uleb(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
                Ok(())
            }
            Value::Bytes(bytes) => {
                out.push(0); // homogenouslySized
                push_uleb(out, bytes.len() as u64);
                push_uleb(out, 1);
                out.extend_from_slice(bytes);
                Ok(())
            }
            Value::Array(values) => {
                let mut encoded = Vec::with_capacity(values.len());
                for value in values {
                    if matches!(value, Value::Null) {
                        encoded.push(None);
                    } else {
                        let mut buf = Vec::new();
                        self.encode(value, &mut buf)?;
                        encoded.push(Some(buf));
                    }
                }
                if encoded.iter().any(|v| v.is_none()) {
                    out.push(2); // nullable
                    push_uleb(out, encoded.len() as u64);
                    let mut bits = vec![0u8; encoded.len().div_ceil(8)];
                    for (i, value) in encoded.iter().enumerate() {
                        if value.is_none() {
                            bits[i >> 3] |= 1 << (i & 7);
                        }
                    }
                    out.extend_from_slice(&bits);
                    for value in encoded.iter().flatten() {
                        push_uleb(out, value.len() as u64);
                    }
                    for value in encoded.iter().flatten() {
                        out.extend_from_slice(value);
                    }
                } else {
                    let sizes: Vec<usize> = encoded
                        .iter()
                        .map(|v| v.as_ref().map_or(0, Vec::len))
                        .collect();
                    if sizes.windows(2).all(|w| w[0] == w[1]) {
                        out.push(0); // homogenouslySized
                        push_uleb(out, encoded.len() as u64);
                        push_uleb(out, sizes.first().copied().unwrap_or(0) as u64);
                    } else {
                        out.push(1); // heterogeneous
                        push_uleb(out, encoded.len() as u64);
                        for size in &sizes {
                            push_uleb(out, *size as u64);
                        }
                    }
                    for value in encoded.iter().flatten() {
                        out.extend_from_slice(value);
                    }
                }
                Ok(())
            }
            Value::Dict(map) => {
                // BTreeMap guarantees the same sorted-key order used by
                // pyutmremote's encoder.
                let keys: Vec<String> = map.keys().cloned().collect();
                let id = self.shape_id(&keys);

                let mut encoded: Vec<Option<Vec<u8>>> = Vec::with_capacity(keys.len());
                for key in &keys {
                    match map.get(key) {
                        Some(Value::Null) | None => encoded.push(None),
                        Some(value) => {
                            let mut buf = Vec::new();
                            self.encode(value, &mut buf)?;
                            encoded.push(Some(buf));
                        }
                    }
                }

                if encoded.iter().all(|v| v.is_some()) {
                    out.push(0); // nonnull
                    push_uleb(out, id);
                } else {
                    out.push(1); // nullable
                    push_uleb(out, id);
                    let mut bits = vec![0u8; keys.len().div_ceil(8)];
                    for (i, value) in encoded.iter().enumerate() {
                        if value.is_none() {
                            bits[i >> 3] |= 1 << (i & 7);
                        }
                    }
                    out.extend_from_slice(&bits);
                }
                for value in encoded.iter().flatten() {
                    push_uleb(out, value.len() as u64);
                }
                for value in encoded.iter().flatten() {
                    out.extend_from_slice(value);
                }
                Ok(())
            }
        }
    }
}

/// Encode a Codable value (including its shape table).
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    let mut encoder = Encoder::new();
    let mut body = Vec::new();
    encoder.encode(value, &mut body)?;

    let mut out = Vec::with_capacity(body.len() + 64);
    push_uleb(&mut out, encoder.shapes.len() as u64);
    for shape in &encoder.shapes {
        push_uleb(&mut out, shape.len() as u64);
        for key in shape {
            push_uleb(&mut out, key.len() as u64);
            out.extend_from_slice(key.as_bytes());
        }
    }
    out.extend_from_slice(&body);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Schema used while decoding.  UTM's replies are strongly typed, but the wire
/// format does not carry type information, so we describe the fields we care
/// about.  `Skip` fields are measured but not decoded.
#[derive(Debug, Clone, Copy)]
pub enum Type {
    Bool,
    Int,
    Str,
    Bytes,
    Enum,
    Skip,
    Array(&'static Type),
    Struct(&'static [(&'static str, Type)]),
    MapStrStr,
}

pub static T_BOOL: Type = Type::Bool;
pub static T_INT: Type = Type::Int;
pub static T_STR: Type = Type::Str;
pub static T_BYTES: Type = Type::Bytes;
pub static T_ENUM: Type = Type::Enum;
pub static T_SKIP: Type = Type::Skip;
pub static T_MAP_STR_STR: Type = Type::MapStrStr;

fn read_bit(bits: &[u8], index: usize) -> bool {
    bits.get(index >> 3)
        .map(|b| (b >> (index & 7)) & 1 != 0)
        .unwrap_or(false)
}

struct Decoder<'a> {
    shapes: &'a ShapeTable,
}

impl<'a> Decoder<'a> {
    fn value(&self, data: &'a [u8], ty: Type) -> Result<Value> {
        match ty {
            Type::Skip => Ok(Value::Null),
            Type::Bool => {
                let mut r = Reader::new(data);
                Ok(Value::Bool(r.byte()? != 0))
            }
            Type::Int => {
                let mut r = Reader::new(data);
                let n = r.remaining().min(8);
                let bytes = r.take(n)?;
                let mut buf = [0u8; 8];
                buf[..n].copy_from_slice(bytes);
                let mut value = i64::from_le_bytes(buf);
                // Sign extend values shorter than 8 bytes.
                if n < 8 && bytes.last().is_some_and(|b| b & 0x80 != 0) {
                    value |= -1i64 << (n * 8);
                }
                Ok(Value::Int(value))
            }
            Type::Str => {
                let mut r = Reader::new(data);
                let len = r.uleb()? as usize;
                let bytes = r.take(len)?;
                Ok(Value::Str(
                    std::str::from_utf8(bytes)
                        .map_err(|_| Error::Codable("string is not UTF-8".into()))?
                        .to_owned(),
                ))
            }
            Type::Bytes => {
                let mut r = Reader::new(data);
                let meta = r.byte()?;
                if meta != 0 {
                    return Err(Error::Codable(format!(
                        "bytes container has unexpected metadata {meta}"
                    )));
                }
                let count = r.uleb()? as usize;
                let size = r.uleb()? as usize;
                let total = count
                    .checked_mul(size)
                    .ok_or_else(|| Error::Codable("bytes length overflow".into()))?;
                Ok(Value::Bytes(r.take(total)?.to_vec()))
            }
            Type::Enum => self.enum_value(data),
            Type::Array(elem) => self.array(data, *elem),
            Type::Struct(fields) => self.struct_value(data, fields),
            Type::MapStrStr => {
                let dict = self.keyed(data, |_| T_STR)?;
                Ok(dict)
            }
        }
    }

    fn enum_value(&self, data: &'a [u8]) -> Result<Value> {
        let mut r = Reader::new(data);
        let meta = r.byte()?;
        let shape_id = r.uleb()?;
        let keys = self.shapes.get(shape_id)?;
        if keys.len() != 1 {
            return Err(Error::Codable(format!(
                "enum container has {} keys",
                keys.len()
            )));
        }
        let nulls = if meta == 1 {
            let n = keys.len().div_ceil(8);
            Some(r.take(n)?.to_vec())
        } else if meta == 0 {
            None
        } else {
            return Err(Error::Codable(format!(
                "enum container has unexpected metadata {meta}"
            )));
        };
        let is_null = nulls.as_ref().is_some_and(|bits| read_bit(bits, 0));
        if is_null {
            return Ok(Value::Null);
        }
        // Read the single value length and ignore the value itself.
        let _len = r.uleb()?;
        Ok(Value::Str(keys[0].clone()))
    }

    fn array(&self, data: &'a [u8], elem: Type) -> Result<Value> {
        let mut r = Reader::new(data);
        let meta = r.byte()?;
        let count = r.uleb()? as usize;
        let mut values = Vec::with_capacity(count);
        match meta {
            0 => {
                let size = r.uleb()? as usize;
                for _ in 0..count {
                    let slice = r.take(size)?;
                    values.push(self.value(slice, elem)?);
                }
            }
            1 => {
                let mut sizes = Vec::with_capacity(count);
                for _ in 0..count {
                    sizes.push(r.uleb()? as usize);
                }
                for size in sizes {
                    let slice = r.take(size)?;
                    values.push(self.value(slice, elem)?);
                }
            }
            2 => {
                let bits_len = count.div_ceil(8);
                let bits = r.take(bits_len)?.to_vec();
                let mut sizes = Vec::with_capacity(count);
                for i in 0..count {
                    if !read_bit(&bits, i) {
                        sizes.push(r.uleb()? as usize);
                    } else {
                        sizes.push(usize::MAX);
                    }
                }
                for size in sizes {
                    if size == usize::MAX {
                        values.push(Value::Null);
                    } else {
                        let slice = r.take(size)?;
                        values.push(self.value(slice, elem)?);
                    }
                }
            }
            other => {
                return Err(Error::Codable(format!(
                    "array container has unexpected metadata {other}"
                )))
            }
        }
        Ok(Value::Array(values))
    }

    fn keyed<F>(&self, data: &'a [u8], ty_for: F) -> Result<Value>
    where
        F: Fn(&str) -> Type,
    {
        let mut r = Reader::new(data);
        let meta = r.byte()?;
        let shape_id = r.uleb()?;
        let keys = self.shapes.get(shape_id)?;
        let nulls = match meta {
            0 => None,
            1 => {
                let n = keys.len().div_ceil(8);
                Some(r.take(n)?.to_vec())
            }
            other => {
                return Err(Error::Codable(format!(
                    "keyed container has unexpected metadata {other}"
                )))
            }
        };
        let mut lengths = Vec::with_capacity(keys.len());
        for (i, _) in keys.iter().enumerate() {
            let is_null = nulls.as_ref().is_some_and(|bits| read_bit(bits, i));
            if is_null {
                lengths.push(None);
            } else {
                lengths.push(Some(r.uleb()? as usize));
            }
        }
        let mut map = BTreeMap::new();
        for (key, len) in keys.iter().zip(lengths) {
            match len {
                None => {
                    map.insert(key.clone(), Value::Null);
                }
                Some(len) => {
                    let slice = r.take(len)?;
                    let value = self.value(slice, ty_for(key))?;
                    if !matches!(value, Value::Null) {
                        map.insert(key.clone(), value);
                    }
                }
            }
        }
        Ok(Value::Dict(map))
    }

    fn struct_value(
        &self,
        data: &'a [u8],
        fields: &'static [(&'static str, Type)],
    ) -> Result<Value> {
        self.keyed(data, |key| {
            fields
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, ty)| *ty)
                .unwrap_or(Type::Skip)
        })
    }
}

/// Decode a payload with the supplied schema.
pub fn decode(data: &[u8], ty: Type) -> Result<Value> {
    let mut r = Reader::new(data);
    let shapes = ShapeTable::parse(&mut r)?;
    let rest = &data[r.pos..];
    let decoder = Decoder { shapes: &shapes };
    decoder.value(rest, ty)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn encodes_handshake_like_pyutmremote() {
        let value = Value::dict([
            ("version", Value::Int(1)),
            ("password", Value::Str("secret".into())),
        ]);
        assert_eq!(
            hex(&encode(&value).unwrap()),
            "01020870617373776f72640776657273696f6e00000708067365637265740100000000000000"
        );
    }

    #[test]
    fn encodes_empty_reply_like_pyutmremote() {
        assert_eq!(hex(&encode(&Value::empty()).unwrap()), "01000000");
    }

    #[test]
    fn encodes_uuid_list_like_pyutmremote() {
        let value = Value::dict([(
            "ids",
            Value::Array(vec![Value::Str("A".into()), Value::Str("B".into())]),
        )]);
        assert_eq!(
            hex(&encode(&value).unwrap()),
            "01010369647300000700020201410142"
        );
    }

    #[test]
    fn encodes_start_request_like_pyutmremote() {
        let value = Value::dict([
            ("id", Value::Str("ABC-123".into())),
            ("options", Value::Int(4)),
        ]);
        assert_eq!(
            hex(&encode(&value).unwrap()),
            "0102026964076f7074696f6e7300000808074142432d3132330400000000000000"
        );
    }

    #[test]
    fn decodes_nested_reply() {
        static INFO: Type = Type::Struct(&[
            ("id", Type::Str),
            ("name", Type::Str),
            ("state", Type::Enum),
            ("isTakeoverAllowed", Type::Bool),
            ("mountedDrives", Type::MapStrStr),
        ]);
        static INFOS: Type = Type::Array(&INFO);
        static REPLY: Type = Type::Struct(&[("informations", INFOS)]);

        // Generated with pyutmremote for a VirtualMachineInformation reply.
        let info = Value::dict([
            ("id", Value::Str("vm-1".into())),
            ("name", Value::Str("Linux".into())),
            ("state", Value::dict([("started", Value::empty())])),
            ("isTakeoverAllowed", Value::Bool(true)),
            (
                "mountedDrives",
                Value::dict([("disk.img", Value::Str("/dev/sda".into()))]),
            ),
        ]);
        let encoded = encode(&Value::dict([("informations", Value::Array(vec![info]))])).unwrap();

        let decoded = decode(&encoded, REPLY).unwrap();
        let infos = decoded.get("informations").unwrap().as_array().unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].get("name").unwrap().as_str(), Some("Linux"));
        assert_eq!(infos[0].get("state").unwrap().as_str(), Some("started"));
        assert_eq!(
            infos[0].get("isTakeoverAllowed").unwrap().as_bool(),
            Some(true)
        );
        assert_eq!(
            infos[0]
                .get("mountedDrives")
                .unwrap()
                .get("disk.img")
                .unwrap()
                .as_str(),
            Some("/dev/sda")
        );
    }

    #[test]
    fn decodes_bytes() {
        static REPLY: Type = Type::Struct(&[("data", Type::Bytes)]);
        let encoded = encode(&Value::dict([("data", Value::Bytes(vec![0, 1, 2, 255]))])).unwrap();
        let decoded = decode(&encoded, REPLY).unwrap();
        assert_eq!(
            decoded.get("data").unwrap().as_bytes(),
            Some(&[0u8, 1, 2, 255][..])
        );
    }

    #[test]
    fn roundtrip_heterogeneous_array() {
        static ARR: Type = Type::Array(&Type::Str);
        let value = Value::Array(vec![Value::Str("a".into()), Value::Str("long".into())]);
        let encoded = encode(&value).unwrap();
        let decoded = decode(&encoded, ARR).unwrap();
        assert_eq!(decoded, value);
    }
}
