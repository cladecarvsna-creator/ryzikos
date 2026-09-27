//! TL, the binary format Telegram speaks: every object is a 32-bit
//! constructor number followed by its fields, all little endian and
//! padded to 4 bytes.
//!
//! Instead of a hand-written struct per object, the schema (schema.tl,
//! the same text the official clients are generated from) is read at
//! run time. Objects become [`Obj`]s whose fields are looked up by name,
//! so any object the server sends can be read, even ones the app does
//! not use, and requests are built the same way by name.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

pub const VECTOR: u32 = 0x1cb5_c415;
pub const BOOL_TRUE: u32 = 0x9972_75b5;
pub const BOOL_FALSE: u32 = 0xbc79_9737;
pub const GZIP_PACKED: u32 = 0x3072_cfa1;
pub const MSG_CONTAINER: u32 = 0x73f1_f8dc;
pub const RPC_RESULT: u32 = 0xf35c_6d01;

/// How a field is written.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Int,
    Long,
    Double,
    /// `bytes` and `string` are written the same way.
    Bytes,
    Int128,
    Int256,
    /// A `flags:#` field: which of the optional fields are there.
    Flags,
    /// `true` behind a flag: only the flag bit says it.
    True,
    Bool,
    /// `Vector<T>`, with the vector constructor in front.
    Vector(Box<Kind>),
    /// `vector<T>`, without it.
    BareVector(Box<Kind>),
    /// Any object of a type, with its constructor number in front.
    Boxed,
    /// One exact constructor, without its number.
    Bare(u32),
}

#[derive(Debug)]
pub struct Param {
    pub name: String,
    pub kind: Kind,
    /// Optional fields: which flags field (an index into the params)
    /// and which bit of it.
    pub flag: Option<(usize, u32)>,
}

#[derive(Debug)]
pub struct Ctor {
    pub id: u32,
    pub name: String,
    pub params: Vec<Param>,
    /// What a function returns, or the type a constructor makes.
    pub result: Kind,
}

pub struct Schema {
    ctors: BTreeMap<u32, Ctor>,
    by_name: BTreeMap<String, u32>,
}

static SCHEMA_TEXT: &str = include_str!("schema.tl");
static SCHEMA: AtomicPtr<Schema> = AtomicPtr::new(core::ptr::null_mut());

/// The schema, read the first time it is needed.
pub fn schema() -> &'static Schema {
    let p = SCHEMA.load(Ordering::Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    let s = Box::leak(Box::new(Schema::parse(SCHEMA_TEXT)));
    SCHEMA.store(s, Ordering::Release);
    s
}

impl Schema {
    fn parse(text: &str) -> Schema {
        let mut s = Schema {
            ctors: BTreeMap::new(),
            by_name: BTreeMap::new(),
        };
        // bare types name a constructor that may come later: resolve them
        // once everything is read
        // types and functions are read the same way
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.starts_with("//") && l.ends_with(';'))
            .collect();
        for line in &lines {
            if let Some((name, id)) = head(line) {
                s.by_name.insert(String::from(name), id);
            }
        }
        for line in lines {
            if let Some(c) = s.parse_line(line) {
                s.ctors.insert(c.id, c);
            }
        }
        s
    }

    fn parse_line(&self, line: &str) -> Option<Ctor> {
        let (name, id) = head(line)?;
        let (left, right) = line.trim_end_matches(';').split_once(" = ")?;
        let mut params: Vec<Param> = Vec::new();
        for part in left.split_whitespace().skip(1) {
            if part.starts_with('{') {
                continue; // {X:Type}
            }
            let (pname, mut ty) = part.split_once(':')?;
            let mut flag = None;
            if let Some((cond, rest)) = ty.split_once('?') {
                let (field, bit) = cond.split_once('.')?;
                let index = params.iter().position(|p| p.name == field)?;
                flag = Some((index, bit.parse().ok()?));
                ty = rest;
            }
            let kind = if ty == "true" && flag.is_some() {
                Kind::True
            } else {
                self.kind(ty)
            };
            params.push(Param {
                name: String::from(pname),
                kind,
                flag,
            });
        }
        Some(Ctor {
            id,
            name: String::from(name),
            params,
            result: self.kind(right.trim()),
        })
    }

    /// Understand a type as it is written in the schema.
    fn kind(&self, ty: &str) -> Kind {
        let ty = ty.trim_start_matches('%').trim_start_matches('!');
        match ty {
            "int" => Kind::Int,
            "long" => Kind::Long,
            "double" => Kind::Double,
            "bytes" | "string" => Kind::Bytes,
            "int128" => Kind::Int128,
            "int256" => Kind::Int256,
            "#" => Kind::Flags,
            "Bool" => Kind::Bool,
            _ => {
                if let Some(inner) = ty.strip_prefix("Vector<") {
                    return Kind::Vector(Box::new(self.kind(inner.trim_end_matches('>'))));
                }
                if let Some(inner) = ty.strip_prefix("vector<") {
                    return Kind::BareVector(Box::new(self.kind(inner.trim_end_matches('>'))));
                }
                // `Type` and `namespace.Type` are boxed, `ctor` is bare
                let last = ty.rsplit('.').next().unwrap_or(ty);
                if last.starts_with(|c: char| c.is_ascii_lowercase()) {
                    if let Some(&id) = self.by_name.get(ty) {
                        return Kind::Bare(id);
                    }
                }
                Kind::Boxed
            }
        }
    }

    pub fn get(&self, id: u32) -> Option<&Ctor> {
        self.ctors.get(&id)
    }

    pub fn by_name(&self, name: &str) -> Option<&Ctor> {
        self.by_name.get(name).and_then(|id| self.ctors.get(id))
    }
}

/// The name and number at the start of a line: `name#1a2b3c4d ...`.
fn head(line: &str) -> Option<(&str, u32)> {
    let word = line.split_whitespace().next()?;
    let (name, id) = word.split_once('#')?;
    Some((name, u32::from_str_radix(id, 16).ok()?))
}

// ---- values ---------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// An optional field that is not there.
    None,
    Int(i32),
    Long(i64),
    Double(f64),
    Bytes(Vec<u8>),
    Bool(bool),
    Vector(Vec<Value>),
    Obj(Box<Obj>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Obj {
    pub id: u32,
    /// In the order of the schema.
    pub fields: Vec<Value>,
}

static NONE: Value = Value::None;

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Bytes(s.as_bytes().to_vec())
    }

    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> i64 {
        match *self {
            Value::Int(v) => v as i64,
            Value::Long(v) => v,
            _ => 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Value::Bytes(b) => b,
            _ => &[],
        }
    }

    pub fn as_vec(&self) -> &[Value] {
        match self {
            Value::Vector(v) => v,
            _ => &[],
        }
    }

    pub fn is_some(&self) -> bool {
        !matches!(self, Value::None | Value::Bool(false))
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Value {
        Value::Int(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Value {
        Value::Long(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::str(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}

impl From<Obj> for Value {
    fn from(v: Obj) -> Value {
        Value::Obj(Box::new(v))
    }
}

impl Obj {
    /// An object by constructor name, with some of its fields; the rest
    /// are left out (optional) or zero.
    pub fn new(name: &str, fields: &[(&str, Value)]) -> Obj {
        let ctor = schema()
            .by_name(name)
            .unwrap_or_else(|| panic!("tl: no constructor {}", name));
        let mut values = alloc::vec![Value::None; ctor.params.len()];
        for (key, value) in fields {
            match ctor.params.iter().position(|p| p.name == *key) {
                Some(i) => values[i] = value.clone(),
                None => panic!("tl: {} has no field {}", name, key),
            }
        }
        Obj {
            id: ctor.id,
            fields: values,
        }
    }

    pub fn ctor(&self) -> &'static Ctor {
        schema()
            .get(self.id)
            .expect("tl: object of unknown constructor")
    }

    pub fn name(&self) -> &'static str {
        schema().get(self.id).map_or("?", |c| c.name.as_str())
    }

    pub fn is(&self, name: &str) -> bool {
        self.name() == name
    }

    pub fn get(&self, field: &str) -> &Value {
        let ctor = self.ctor();
        match ctor.params.iter().position(|p| p.name == field) {
            Some(i) => &self.fields[i],
            None => &NONE,
        }
    }

    pub fn int(&self, field: &str) -> i64 {
        self.get(field).as_i64()
    }

    pub fn bytes(&self, field: &str) -> &[u8] {
        self.get(field).as_bytes()
    }

    /// A text field, with broken UTF-8 replaced.
    pub fn string(&self, field: &str) -> String {
        String::from_utf8_lossy(self.bytes(field)).into_owned()
    }

    pub fn obj(&self, field: &str) -> Option<&Obj> {
        self.get(field).as_obj()
    }

    pub fn vec(&self, field: &str) -> &[Value] {
        self.get(field).as_vec()
    }

    /// Whether a `true` flag (or a Bool) is set.
    pub fn flag(&self, field: &str) -> bool {
        self.get(field).is_some()
    }
}

// ---- writing ----------------------------------------------------------------------

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// A string or bytes: a length, the data and padding to 4 bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        let head = if b.len() < 254 {
            self.buf.push(b.len() as u8);
            1
        } else {
            self.buf.push(254);
            self.buf
                .extend_from_slice(&(b.len() as u32).to_le_bytes()[..3]);
            4
        };
        self.buf.extend_from_slice(b);
        let pad = (4 - (head + b.len()) % 4) % 4;
        self.buf.extend(core::iter::repeat_n(0, pad));
    }

    /// An object with its constructor number.
    pub fn obj(&mut self, o: &Obj) -> Result<(), String> {
        self.u32(o.id);
        self.bare(o)
    }

    /// An object's fields.
    pub fn bare(&mut self, o: &Obj) -> Result<(), String> {
        let ctor = schema().get(o.id).ok_or("tl: unknown constructor")?;
        for (i, p) in ctor.params.iter().enumerate() {
            let v = &o.fields[i];
            if p.kind == Kind::Flags {
                let mut flags = 0u32;
                for (j, q) in ctor.params.iter().enumerate() {
                    if let Some((f, bit)) = q.flag {
                        if f == i && present(q, &o.fields[j]) {
                            flags |= 1 << bit;
                        }
                    }
                }
                self.u32(flags);
                continue;
            }
            if p.flag.is_some() && (!present(p, v) || p.kind == Kind::True) {
                continue;
            }
            self.value(&p.kind, v)
                .map_err(|e| alloc::format!("{}.{}: {}", ctor.name, p.name, e))?;
        }
        Ok(())
    }

    pub fn value(&mut self, kind: &Kind, v: &Value) -> Result<(), String> {
        match (kind, v) {
            (Kind::Int, _) => self.i32(v.as_i64() as i32),
            (Kind::Long, _) => self.i64(v.as_i64()),
            (Kind::Double, Value::Double(d)) => self.raw(&d.to_le_bytes()),
            (Kind::Double, _) => self.raw(&[0; 8]),
            (Kind::Bytes, _) => self.bytes(v.as_bytes()),
            (Kind::Int128 | Kind::Int256, Value::Bytes(b)) => self.raw(b),
            (Kind::Int128, _) => self.raw(&[0; 16]),
            (Kind::Int256, _) => self.raw(&[0; 32]),
            (Kind::Bool, _) => self.u32(if v.is_some() { BOOL_TRUE } else { BOOL_FALSE }),
            (Kind::True | Kind::Flags, _) => {}
            (Kind::Vector(inner), _) => {
                self.u32(VECTOR);
                self.vector(inner, v.as_vec())?;
            }
            (Kind::BareVector(inner), _) => self.vector(inner, v.as_vec())?,
            (Kind::Boxed, Value::Obj(o)) => self.obj(o)?,
            (Kind::Bare(_), Value::Obj(o)) => self.bare(o)?,
            (Kind::Boxed | Kind::Bare(_), _) => return Err(String::from("missing object")),
        }
        Ok(())
    }

    fn vector(&mut self, inner: &Kind, items: &[Value]) -> Result<(), String> {
        self.u32(items.len() as u32);
        for item in items {
            self.value(inner, item)?;
        }
        Ok(())
    }
}

/// Whether an optional field is there: a `true` flag must be set, any
/// other field just given.
fn present(p: &Param, v: &Value) -> bool {
    match p.kind {
        Kind::True => v.is_some(),
        _ => *v != Value::None,
    }
}

/// A request or object as bytes.
pub fn encode(o: &Obj) -> Vec<u8> {
    let mut w = Writer::new();
    w.obj(o).expect("tl: can't encode");
    w.buf
}

// ---- reading ------------------------------------------------------------------------

pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

type R<T> = Result<T, String>;

fn short() -> String {
    String::from("tl: data ends early")
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(short)?;
        let s = self.data.get(self.pos..end).ok_or_else(short)?;
        self.pos = end;
        Ok(s)
    }

    pub fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn i32(&mut self) -> R<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn i64(&mut self) -> R<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn bytes(&mut self) -> R<&'a [u8]> {
        let first = self.take(1)?[0];
        let (len, head) = if first == 254 {
            let b = self.take(3)?;
            (
                b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16,
                4,
            )
        } else {
            (first as usize, 1)
        };
        let data = self.take(len)?;
        self.take((4 - (head + len) % 4) % 4)?;
        Ok(data)
    }

    /// An object of any type, with its constructor number.
    pub fn obj(&mut self) -> R<Obj> {
        match self.boxed(&Kind::Boxed)? {
            Value::Obj(o) => Ok(*o),
            _ => Err(String::from("tl: expected an object")),
        }
    }

    /// A value of a boxed type: read its constructor number first.
    /// `kind` says what to expect if it is a vector.
    pub fn boxed(&mut self, kind: &Kind) -> R<Value> {
        let id = self.u32()?;
        match id {
            BOOL_TRUE => Ok(Value::Bool(true)),
            BOOL_FALSE => Ok(Value::Bool(false)),
            GZIP_PACKED => {
                let packed = self.bytes()?;
                let data = gunzip(packed)?;
                Reader::new(&data).boxed(kind)
            }
            VECTOR => {
                let inner = match kind {
                    Kind::Vector(inner) => (**inner).clone(),
                    _ => Kind::Boxed,
                };
                self.items(&inner)
            }
            _ => {
                let ctor = schema()
                    .get(id)
                    .ok_or_else(|| alloc::format!("tl: unknown constructor {:08x}", id))?;
                Ok(Value::Obj(Box::new(self.fields(ctor)?)))
            }
        }
    }

    fn items(&mut self, inner: &Kind) -> R<Value> {
        let n = self.u32()? as usize;
        if n > self.data.len() {
            return Err(short());
        }
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            items.push(self.value(inner)?);
        }
        Ok(Value::Vector(items))
    }

    fn fields(&mut self, ctor: &Ctor) -> R<Obj> {
        let mut fields = Vec::with_capacity(ctor.params.len());
        for p in &ctor.params {
            if let Some((f, bit)) = p.flag {
                let set = matches!(fields[f], Value::Int(flags) if flags as u32 & (1 << bit) != 0);
                if !set {
                    fields.push(Value::None);
                    continue;
                }
                if p.kind == Kind::True {
                    fields.push(Value::Bool(true));
                    continue;
                }
            }
            let v = match p.kind {
                Kind::Flags => Value::Int(self.i32()?),
                _ => self.value(&p.kind)?,
            };
            fields.push(v);
        }
        Ok(Obj {
            id: ctor.id,
            fields,
        })
    }

    pub fn value(&mut self, kind: &Kind) -> R<Value> {
        Ok(match kind {
            Kind::Int | Kind::Flags => Value::Int(self.i32()?),
            Kind::Long => Value::Long(self.i64()?),
            Kind::Double => Value::Double(f64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            Kind::Bytes => Value::Bytes(self.bytes()?.to_vec()),
            Kind::Int128 => Value::Bytes(self.take(16)?.to_vec()),
            Kind::Int256 => Value::Bytes(self.take(32)?.to_vec()),
            Kind::True => Value::Bool(true),
            Kind::Bool | Kind::Boxed | Kind::Vector(_) => self.boxed(kind)?,
            Kind::BareVector(inner) => self.items(inner)?,
            Kind::Bare(id) => {
                let ctor = schema().get(*id).ok_or_else(short)?;
                Value::Obj(Box::new(self.fields(ctor)?))
            }
        })
    }
}

/// Unpack `gzip_packed` data.
pub fn gunzip(data: &[u8]) -> R<Vec<u8>> {
    let options = zune_inflate::DeflateOptions::default().set_limit(64 * 1024 * 1024);
    zune_inflate::DeflateDecoder::new_with_options(data, options)
        .decode_gzip()
        .map_err(|_| String::from("tl: bad gzip data"))
}
