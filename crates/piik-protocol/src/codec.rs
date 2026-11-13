//! Decode and encode primitives shared by every wire type.
//!
//! The decode model: a strict object (zod `.strict()`) rejects unknown keys,
//! presence checks tell "absent" from "null" the way zod's `.optional()` /
//! `.nullable()` pair does, and a member that is absent or null takes its zero
//! value unless a presence check forbids that.
//! Duplicate keys collapse to the last occurrence, matching `JSON.parse`.

use std::sync::LazyLock;

use serde_json::{Map, Number, Value};
use thiserror::Error;

use crate::constants::MAX_SAFE_INTEGER;

/// A JSON number restricted to an integral value inside the safe integer
/// range, mirroring zod's `z.number().int()`: `5.0` is an integer, `5.5` is
/// not.
pub type Int = i64;

/// A finite JSON number, mirroring zod's `z.number().finite()`.
pub type Num = f64;

/// Every decode failure. Schema violations carry this codec's own messages;
/// JSON syntax and type errors carry their own text.
#[derive(Debug, Error)]
pub enum DecodeError {
    /// The input is not one JSON document.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The document does not satisfy the schema.
    #[error("{0}")]
    Invalid(String),
}

pub(crate) type Result<T> = std::result::Result<T, DecodeError>;

pub(crate) fn invalid(message: impl Into<String>) -> DecodeError {
    DecodeError::Invalid(message.into())
}

/// Fails with `message` unless `condition` holds.
pub(crate) fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

/// Parses one JSON document.
pub(crate) fn parse(data: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice(data)?)
}

/// A wire type that can be decoded from a parsed JSON value.
pub trait FromWire: Sized {
    /// Decodes and validates `value`.
    fn from_wire(value: &Value) -> Result<Self>;
}

/// A wire type that can be encoded to JSON.
pub trait ToWire {
    /// Builds the JSON value, with keys in wire order.
    fn to_wire(&self) -> Value;

    /// Serializes without HTML escaping, as `JSON.stringify` does.
    fn to_json(&self) -> Vec<u8> {
        self.to_wire().to_string().into_bytes()
    }
}

/// Parses `data` and decodes one wire value.
pub fn decode<T: FromWire>(data: &[u8]) -> std::result::Result<T, DecodeError> {
    T::from_wire(&parse(data)?)
}

/// Encodes one wire value.
pub fn encode<T: ToWire + ?Sized>(value: &T) -> Vec<u8> {
    value.to_json()
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(crate) fn type_error(expected: &str, value: &Value) -> DecodeError {
    invalid(format!("expected {expected}, got {}", kind_of(value)))
}

static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

/// The raw members of one JSON object.
#[derive(Clone, Copy)]
pub(crate) struct Fields<'a>(&'a Map<String, Value>);

/// Opens one strict object: keys outside `known` are rejected. A JSON null
/// opens as an empty object, so every member takes its zero value.
pub(crate) fn object<'a>(value: &'a Value, known: &[&str]) -> Result<Fields<'a>> {
    let map = match value {
        Value::Object(map) => map,
        Value::Null => &EMPTY,
        other => return Err(type_error("an object", other)),
    };
    if let Some(name) = map.keys().find(|name| !known.contains(&name.as_str())) {
        return Err(invalid(format!("unknown field {name:?}")));
    }
    Ok(Fields(map))
}

/// Opens an object without strictness, for the union discriminator probe.
pub(crate) fn loose_object(value: &Value) -> Result<Fields<'_>> {
    match value {
        Value::Object(map) => Ok(Fields(map)),
        Value::Null => Ok(Fields(&EMPTY)),
        other => Err(type_error("an object", other)),
    }
}

/// Reads the union discriminator without imposing strictness, the way zod
/// tries each member of `z.union` / `z.discriminatedUnion`.
pub(crate) fn discriminator<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    match loose_object(value)?.get(key) {
        Some(Value::String(text)) => Ok(text),
        _ => Err(invalid("no matching message variant")),
    }
}

impl<'a> Fields<'a> {
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.get(key)
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub(crate) fn is_null(&self, key: &str) -> bool {
        matches!(self.0.get(key), Some(Value::Null))
    }

    /// Mirrors a plain required zod field: present and not null.
    pub(crate) fn require(&self, keys: &[&str]) -> Result<()> {
        for key in keys {
            if !self.has(key) {
                return Err(invalid(format!("{key} is required")));
            }
            if self.is_null(key) {
                return Err(invalid(format!("{key} must not be null")));
            }
        }
        Ok(())
    }

    /// Mirrors `.nullable()` without `.optional()`: present, possibly null.
    pub(crate) fn require_nullable(&self, keys: &[&str]) -> Result<()> {
        match keys.iter().find(|key| !self.has(key)) {
            Some(key) => Err(invalid(format!("{key} is required"))),
            None => Ok(()),
        }
    }

    /// Mirrors `.optional()` without `.nullable()`: null is rejected, an
    /// absent key is not.
    pub(crate) fn optional(&self, keys: &[&str]) -> Result<()> {
        match keys.iter().find(|key| self.is_null(key)) {
            Some(key) => Err(invalid(format!("{key} must not be null"))),
            None => Ok(()),
        }
    }

    /// A value member: absent leaves the zero value; null reaches the
    /// decoder (zero for scalars, a validation failure for objects).
    pub(crate) fn or_default<T: FromWire + Default>(&self, key: &str) -> Result<T> {
        match self.get(key) {
            None => Ok(T::default()),
            Some(value) => T::from_wire(value),
        }
    }

    /// A required value member whose presence was already checked.
    pub(crate) fn value<T: FromWire>(&self, key: &str) -> Result<T> {
        match self.get(key) {
            None => Err(invalid(format!("{key} is required"))),
            Some(value) => T::from_wire(value),
        }
    }

    /// An optional member: absent and null are both `None`.
    pub(crate) fn opt<T: FromWire>(&self, key: &str) -> Result<Option<T>> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => T::from_wire(value).map(Some),
        }
    }

    /// A member that is both `.nullable()` and `.optional()`.
    pub(crate) fn nullable<T: FromWire>(&self, key: &str) -> Result<Nullable<T>> {
        match self.get(key) {
            None => Ok(Nullable::Absent),
            Some(Value::Null) => Ok(Nullable::Null),
            Some(value) => T::from_wire(value).map(Nullable::Value),
        }
    }

    pub(crate) fn string(&self, key: &str) -> Result<String> {
        self.or_default(key)
    }

    pub(crate) fn boolean(&self, key: &str) -> Result<bool> {
        self.or_default(key)
    }

    pub(crate) fn int(&self, key: &str) -> Result<Int> {
        self.or_default(key)
    }
}

/// A value that is both `.nullable()` and `.optional()`, so the three wire
/// states survive a decode/encode round trip.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Nullable<T> {
    /// The key is absent and is omitted on encode.
    #[default]
    Absent,
    /// The key carries JSON null.
    Null,
    /// The key carries a value.
    Value(T),
}

impl<T> Nullable<T> {
    /// Returns the carried value, if any.
    pub fn value(&self) -> Option<&T> {
        match self {
            Nullable::Value(value) => Some(value),
            _ => None,
        }
    }
}

impl FromWire for String {
    fn from_wire(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(String::new()),
            Value::String(text) => Ok(text.clone()),
            other => Err(type_error("a string", other)),
        }
    }
}

impl FromWire for bool {
    fn from_wire(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(false),
            Value::Bool(flag) => Ok(*flag),
            other => Err(type_error("a boolean", other)),
        }
    }
}

impl FromWire for Int {
    fn from_wire(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(0),
            Value::Number(number) => safe_integer(number),
            other => Err(type_error("a number", other)),
        }
    }
}

impl FromWire for Num {
    fn from_wire(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(0.0),
            Value::Number(number) => number
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or_else(|| invalid(format!("{number} is not finite"))),
            other => Err(type_error("a number", other)),
        }
    }
}

impl<T: FromWire> FromWire for Vec<T> {
    fn from_wire(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(Vec::new()),
            Value::Array(items) => items.iter().map(T::from_wire).collect(),
            other => Err(type_error("an array", other)),
        }
    }
}

/// Parses the number as a float64 that must be integral and inside the safe
/// integer range.
fn safe_integer(number: &Number) -> Result<Int> {
    let limit = MAX_SAFE_INTEGER as f64;
    match number.as_f64() {
        Some(value)
            if value.is_finite() && value.trunc() == value && (-limit..=limit).contains(&value) =>
        {
            Ok(value as Int)
        }
        _ => Err(invalid(format!("{number} is not a safe integer"))),
    }
}

/// A plain integer member: only an integer literal (no fraction or exponent)
/// inside the int64 range decodes; null decodes as zero.
pub(crate) fn plain_int(value: &Value) -> Result<i64> {
    match value {
        Value::Null => Ok(0),
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| invalid(format!("{number} is not an integer"))),
        other => Err(type_error("a number", other)),
    }
}

pub(crate) fn in_range(value: Int, minimum: Int, maximum: Int) -> bool {
    (minimum..=maximum).contains(&value)
}

pub(crate) fn in_range_num(value: Num, minimum: Num, maximum: Num) -> bool {
    value >= minimum && value <= maximum
}

impl ToWire for str {
    fn to_wire(&self) -> Value {
        Value::String(self.to_owned())
    }
}

impl ToWire for String {
    fn to_wire(&self) -> Value {
        Value::String(self.clone())
    }
}

impl ToWire for bool {
    fn to_wire(&self) -> Value {
        Value::Bool(*self)
    }
}

impl ToWire for Int {
    fn to_wire(&self) -> Value {
        Value::from(*self)
    }
}

impl ToWire for Num {
    /// Integral values encode without a fraction, as `JSON.stringify` does.
    fn to_wire(&self) -> Value {
        const EXACT: f64 = 9_007_199_254_740_992.0;
        if self.trunc() == *self && self.abs() < EXACT {
            Value::from(*self as i64)
        } else {
            Value::from(*self)
        }
    }
}

impl<T: ToWire> ToWire for Option<T> {
    fn to_wire(&self) -> Value {
        self.as_ref().map_or(Value::Null, T::to_wire)
    }
}

impl<T: ToWire> ToWire for Vec<T> {
    fn to_wire(&self) -> Value {
        Value::Array(self.iter().map(T::to_wire).collect())
    }
}

impl<T: ToWire> ToWire for [T] {
    fn to_wire(&self) -> Value {
        Value::Array(self.iter().map(T::to_wire).collect())
    }
}

impl<T: ToWire + ?Sized> ToWire for &T {
    fn to_wire(&self) -> Value {
        (**self).to_wire()
    }
}

/// Builds one JSON object with keys in insertion order.
#[derive(Default)]
pub(crate) struct Obj(Map<String, Value>);

impl Obj {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Starts a message object with its `type` literal.
    pub(crate) fn message(message_type: &str) -> Self {
        Self::new().put("type", message_type)
    }

    pub(crate) fn put<T: ToWire + ?Sized>(mut self, key: &str, value: &T) -> Self {
        self.0.insert(key.to_owned(), value.to_wire());
        self
    }

    /// Emits the key only when `condition` holds.
    pub(crate) fn put_if<T: ToWire + ?Sized>(self, condition: bool, key: &str, value: &T) -> Self {
        if condition {
            self.put(key, value)
        } else {
            self
        }
    }

    /// Emits the key only for `Some`.
    pub(crate) fn put_some<T: ToWire>(self, key: &str, value: &Option<T>) -> Self {
        match value {
            Some(value) => self.put(key, value),
            None => self,
        }
    }

    /// Emits null or the value, and omits an absent key.
    pub(crate) fn put_nullable<T: ToWire>(self, key: &str, value: &Nullable<T>) -> Self {
        match value {
            Nullable::Absent => self,
            Nullable::Null => self.put(key, &Option::<T>::None),
            Nullable::Value(value) => self.put(key, value),
        }
    }

    pub(crate) fn build(self) -> Value {
        Value::Object(self.0)
    }
}

/// Declares a closed string enumeration with its wire spelling.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident = $wire:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            /// The wire spelling.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $wire),+
                }
            }

            /// Parses the wire spelling.
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($wire => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl $crate::codec::ToWire for $name {
            fn to_wire(&self) -> ::serde_json::Value {
                ::serde_json::Value::from(self.as_str())
            }
        }

        impl $crate::codec::FromWire for $name {
            fn from_wire(value: &::serde_json::Value) -> $crate::codec::Result<Self> {
                let text = <String as $crate::codec::FromWire>::from_wire(value)?;
                $name::parse(&text).ok_or_else(|| {
                    $crate::codec::invalid(format!(
                        concat!("unknown ", stringify!($name), " {:?}"),
                        text
                    ))
                })
            }
        }
    };
}

pub(crate) use wire_enum;
