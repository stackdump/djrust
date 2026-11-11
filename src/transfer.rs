//! A compact binary form of serde types (feature `transfer`): what a prepared track is sent in
//! from the engine instance that prepared it to the one that plays it (djweb: a Web Worker
//! prepares, the AudioWorklet installs).
//!
//! Not self-describing and not a storage format: both ends are the same build. Little-endian
//! fixed-width numbers, floats as their bits (so every value arrives exactly), `u32` lengths,
//! `u32` enum variant indices, `u8` option tags, struct fields in declaration order.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use serde::de::{self, DeserializeSeed, IntoDeserializer, SeqAccess, VariantAccess, Visitor};
use serde::{ser, Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// `value` as bytes.
pub fn to_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut s = Ser { out: Vec::with_capacity(1 << 16) };
    value.serialize(&mut s)?;
    Ok(s.out)
}

/// The value `bytes` holds; all of them must be read.
pub fn from_bytes<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    let mut d = De { b: bytes, at: 0 };
    let v = T::deserialize(&mut d)?;
    if d.at != bytes.len() {
        return Err(Error("trailing bytes".into()));
    }
    Ok(v)
}

struct Ser {
    out: Vec<u8>,
}

impl Ser {
    fn len(&mut self, n: usize) -> Result<()> {
        let n = u32::try_from(n).map_err(|_| Error("length over u32".into()))?;
        self.out.extend_from_slice(&n.to_le_bytes());
        Ok(())
    }
}

macro_rules! ser_num {
    ($($f:ident $t:ty),*) => {$(
        fn $f(self, v: $t) -> Result<()> {
            self.out.extend_from_slice(&v.to_le_bytes());
            Ok(())
        }
    )*};
}

impl<'a> ser::Serializer for &'a mut Ser {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    ser_num!(serialize_i8 i8, serialize_i16 i16, serialize_i32 i32, serialize_i64 i64, serialize_i128 i128,
        serialize_u8 u8, serialize_u16 u16, serialize_u32 u32, serialize_u64 u64, serialize_u128 u128);

    fn serialize_bool(self, v: bool) -> Result<()> {
        self.out.push(v as u8);
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<()> {
        self.serialize_u32(v.to_bits())
    }
    fn serialize_f64(self, v: f64) -> Result<()> {
        self.serialize_u64(v.to_bits())
    }
    fn serialize_char(self, v: char) -> Result<()> {
        self.serialize_u32(v as u32)
    }
    fn serialize_str(self, v: &str) -> Result<()> {
        self.serialize_bytes(v.as_bytes())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        self.len(v.len())?;
        self.out.extend_from_slice(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<()> {
        self.out.push(0);
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<()> {
        self.out.push(1);
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<()> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<()> {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, i: u32, _: &'static str) -> Result<()> {
        self.serialize_u32(i)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, v: &T) -> Result<()> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, i: u32, _: &'static str, v: &T) -> Result<()> {
        self.serialize_u32(i)?;
        v.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self> {
        self.len(len.ok_or_else(|| Error("a sequence of unknown length".into()))?)?;
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self> {
        Ok(self)
    }
    fn serialize_tuple_variant(self, _: &'static str, i: u32, _: &'static str, _: usize) -> Result<Self> {
        self.serialize_u32(i)?;
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self> {
        self.len(len.ok_or_else(|| Error("a map of unknown length".into()))?)?;
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self> {
        Ok(self)
    }
    fn serialize_struct_variant(self, _: &'static str, i: u32, _: &'static str, _: usize) -> Result<Self> {
        self.serialize_u32(i)?;
        Ok(self)
    }
    fn is_human_readable(&self) -> bool {
        false
    }
}

macro_rules! ser_compound {
    ($tr:ident, $m:ident) => {
        impl<'a> ser::$tr for &'a mut Ser {
            type Ok = ();
            type Error = Error;
            fn $m<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<()> {
                v.serialize(&mut **self)
            }
            fn end(self) -> Result<()> {
                Ok(())
            }
        }
    };
}
ser_compound!(SerializeSeq, serialize_element);
ser_compound!(SerializeTuple, serialize_element);
ser_compound!(SerializeTupleStruct, serialize_field);
ser_compound!(SerializeTupleVariant, serialize_field);

impl<'a> ser::SerializeMap for &'a mut Ser {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, k: &T) -> Result<()> {
        k.serialize(&mut **self)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> ser::SerializeStruct for &'a mut Ser {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<'a> ser::SerializeStructVariant for &'a mut Ser {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &'static str, v: &T) -> Result<()> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<()> {
        Ok(())
    }
}

struct De<'de> {
    b: &'de [u8],
    at: usize,
}

impl<'de> De<'de> {
    fn take(&mut self, n: usize) -> Result<&'de [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| Error("short input".into()))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn len(&mut self) -> Result<usize> {
        Ok(u32::from_le_bytes(self.arr()?) as usize)
    }
}

macro_rules! de_num {
    ($($f:ident $t:ty, $v:ident);*) => {$(
        fn $f<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            visitor.$v(<$t>::from_le_bytes(self.arr()?))
        }
    )*};
}

impl<'de, 'a> de::Deserializer<'de> for &'a mut De<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value> {
        Err(Error("transfer is not self-describing".into()))
    }

    de_num!(deserialize_i8 i8, visit_i8; deserialize_i16 i16, visit_i16; deserialize_i32 i32, visit_i32;
        deserialize_i64 i64, visit_i64; deserialize_i128 i128, visit_i128;
        deserialize_u8 u8, visit_u8; deserialize_u16 u16, visit_u16; deserialize_u32 u32, visit_u32;
        deserialize_u64 u64, visit_u64; deserialize_u128 u128, visit_u128);

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.take(1)?[0] {
            0 => visitor.visit_bool(false),
            1 => visitor.visit_bool(true),
            _ => Err(Error("bad bool".into())),
        }
    }
    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f32(f32::from_bits(u32::from_le_bytes(self.arr()?)))
    }
    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f64(f64::from_bits(u64::from_le_bytes(self.arr()?)))
    }
    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let c = char::from_u32(u32::from_le_bytes(self.arr()?)).ok_or_else(|| Error("bad char".into()))?;
        visitor.visit_char(c)
    }
    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.len()?;
        let s = core::str::from_utf8(self.take(n)?).map_err(|_| Error("bad utf-8".into()))?;
        visitor.visit_borrowed_str(s)
    }
    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }
    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.len()?;
        visitor.visit_borrowed_bytes(self.take(n)?)
    }
    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_bytes(visitor)
    }
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.take(1)?[0] {
            0 => visitor.visit_none(),
            1 => visitor.visit_some(self),
            _ => Err(Error("bad option tag".into())),
        }
    }
    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }
    fn deserialize_unit_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.len()?;
        visitor.visit_seq(Counted { de: self, left: n })
    }
    fn deserialize_tuple<V: Visitor<'de>>(self, n: usize, visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Counted { de: self, left: n })
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(self, _: &'static str, n: usize, visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Counted { de: self, left: n })
    }
    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let n = self.len()?;
        visitor.visit_map(Counted { de: self, left: n })
    }
    fn deserialize_struct<V: Visitor<'de>>(self, _: &'static str, fields: &'static [&'static str], visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Counted { de: self, left: fields.len() })
    }
    fn deserialize_enum<V: Visitor<'de>>(self, _: &'static str, _: &'static [&'static str], visitor: V) -> Result<V::Value> {
        visitor.visit_enum(self)
    }
    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_u32(u32::from_le_bytes(self.arr()?))
    }
    fn deserialize_ignored_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value> {
        Err(Error("transfer is not self-describing".into()))
    }
    fn is_human_readable(&self) -> bool {
        false
    }
}

struct Counted<'a, 'de> {
    de: &'a mut De<'de>,
    left: usize,
}

impl<'de, 'a> SeqAccess<'de> for Counted<'a, 'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.left)
    }
}

impl<'de, 'a> de::MapAccess<'de> for Counted<'a, 'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        seed.deserialize(&mut *self.de)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.left)
    }
}

impl<'de, 'a> de::EnumAccess<'de> for &'a mut De<'de> {
    type Error = Error;
    type Variant = Self;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        let i = u32::from_le_bytes(self.arr()?);
        let v = seed.deserialize(i.into_deserializer())?;
        Ok((v, self))
    }
}

impl<'de, 'a> VariantAccess<'de> for &'a mut De<'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<()> {
        Ok(())
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(self)
    }
    fn tuple_variant<V: Visitor<'de>>(self, n: usize, visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Counted { de: self, left: n })
    }
    fn struct_variant<V: Visitor<'de>>(self, fields: &'static [&'static str], visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Counted { de: self, left: fields.len() })
    }
}

/// `#[serde(with = "crate::transfer::arr")]`: a fixed array of any length (serde's own impls
/// stop at 32).
pub mod arr {
    use core::marker::PhantomData;
    use serde::de::{Error as _, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize, const N: usize>(a: &[T; N], s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(N)?;
        for x in a {
            t.serialize_element(x)?;
        }
        t.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de> + Copy + Default, const N: usize>(d: D) -> Result<[T; N], D::Error> {
        struct V<T, const N: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de> + Copy + Default, const N: usize> Visitor<'de> for V<T, N> {
            type Value = [T; N];
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "an array of {N}")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<[T; N], A::Error> {
                let mut out = [T::default(); N];
                for (i, x) in out.iter_mut().enumerate() {
                    *x = a.next_element()?.ok_or_else(|| A::Error::invalid_length(i, &self))?;
                }
                Ok(out)
            }
        }
        d.deserialize_tuple(N, V::<T, N>(PhantomData))
    }
}

/// Values one after another, each its own length-prefixed frame (`u32` LE), so the receiver can
/// take them in a few at a time ([`FrameReader`]).
#[derive(Default)]
pub struct Frames {
    pub bytes: Vec<u8>,
}

impl Frames {
    pub fn new() -> Frames {
        Frames::default()
    }

    pub fn push<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<()> {
        let b = to_bytes(v)?;
        let len = u32::try_from(b.len()).map_err(|_| Error("frame too long".into()))?;
        self.bytes.extend_from_slice(&len.to_le_bytes());
        self.bytes.extend_from_slice(&b);
        Ok(())
    }
}

/// Reads [`Frames`] in order; owns its bytes.
pub struct FrameReader {
    bytes: Vec<u8>,
    pos: usize,
}

impl FrameReader {
    pub fn new(bytes: Vec<u8>) -> FrameReader {
        FrameReader { bytes, pos: 0 }
    }

    /// Bytes read so far.
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn at_end(&self) -> bool {
        self.pos == self.bytes.len()
    }

    /// Past the next frame.
    pub fn skip(&mut self) -> Result<()> {
        let len = self.peek_len().ok_or_else(|| Error("no more frames".into()))?;
        if self.pos + 4 + len > self.bytes.len() {
            return Err(Error("frame cut short".into()));
        }
        self.pos += 4 + len;
        Ok(())
    }

    /// The length of the next frame, if there is one.
    pub fn peek_len(&self) -> Option<usize> {
        let h = self.bytes.get(self.pos..self.pos + 4)?;
        Some(u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize)
    }

    pub fn next<T: serde::de::DeserializeOwned>(&mut self) -> Result<T> {
        let len = self.peek_len().ok_or_else(|| Error("no more frames".into()))?;
        let start = self.pos + 4;
        let body = self.bytes.get(start..start + len).ok_or_else(|| Error("frame cut short".into()))?;
        let v = from_bytes(body)?;
        self.pos = start + len;
        Ok(v)
    }
}

/// A [`Project`](crate::pflow::Project) in frames: the project without its nets, then each net
/// (id and bundle). Taken in by [`ProjectBuild`].
pub fn write_project(p: &crate::pflow::Project, f: &mut Frames) -> Result<()> {
    f.push(&(p.nets.len() as u64, &project_shell(p)))?;
    for (id, nb) in p.nets.iter() {
        f.push(&(id, nb))?;
    }
    Ok(())
}

/// `p` without its nets.
pub fn project_shell(p: &crate::pflow::Project) -> crate::pflow::Project {
    // (every field but the nets: a field added to `Project` has to be added here)
    crate::pflow::Project {
        name: p.name.clone(),
        tempo: p.tempo,
        swing: p.swing,
        humanize: p.humanize,
        root_note: p.root_note,
        scale_name: p.scale_name.clone(),
        bars: p.bars,
        nets: crate::pflow::OrderedMap::new(),
        structure: p.structure.clone(),
        initial_mutes: p.initial_mutes.clone(),
        cohesion: p.cohesion.clone(),
        stingers: p.stingers.clone(),
    }
}

/// A project being taken in from [`write_project`]'s frames, one net per [`step`](Self::step).
pub struct ProjectBuild {
    project: crate::pflow::Project,
    nets: usize,
}

impl ProjectBuild {
    pub fn start(r: &mut FrameReader) -> Result<ProjectBuild> {
        let (nets, project): (u64, crate::pflow::Project) = r.next()?;
        Ok(ProjectBuild { project, nets: nets as usize })
    }

    pub fn done(&self) -> bool {
        self.project.nets.len() == self.nets
    }

    pub fn step(&mut self, r: &mut FrameReader) -> Result<()> {
        if self.done() {
            return Err(Error("no net wanted".into()));
        }
        let (id, nb): (String, crate::pflow::NetBundle) = r.next()?;
        if self.project.nets.contains_key(&id) {
            return Err(Error("a net twice".into()));
        }
        self.project.nets.insert(id, nb);
        Ok(())
    }

    pub fn finish(self) -> Result<crate::pflow::Project> {
        if !self.done() {
            return Err(Error("nets missing".into()));
        }
        Ok(self.project)
    }
}

/// A `Vec` that is all zeros (`T::default()`) on the wire as its length alone: the fixed-capacity
/// scratch and history buffers a compiled graph carries before it plays. `serde(with =
/// "crate::transfer::zeros")`.
pub mod zeros {
    use alloc::vec::Vec;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize + Default + PartialEq>(v: &Vec<T>, s: S) -> Result<S::Ok, S::Error> {
        let zero = T::default();
        let data = if v.iter().all(|x| *x == zero) { None } else { Some(v) };
        (v.len() as u64, data).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default + Clone>(d: D) -> Result<Vec<T>, D::Error> {
        let (len, data) = <(u64, Option<Vec<T>>)>::deserialize(d)?;
        Ok(data.unwrap_or_else(|| alloc::vec![T::default(); len as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::sync::Arc;
    use alloc::vec;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    enum E {
        A,
        B(i64),
        C { x: f64, y: Option<String> },
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct S {
        a: Vec<E>,
        m: BTreeMap<String, (u8, f32)>,
        t: Arc<[f32]>,
        nan_bits: f64,
        arr: [i16; 3],
    }

    #[test]
    fn round_trips_exactly() {
        let s = S {
            a: vec![E::A, E::B(-7), E::C { x: 0.1 + 0.2, y: Some("ü".into()) }, E::C { x: -0.0, y: None }],
            m: [("k".to_string(), (3, 1.5f32))].into_iter().collect(),
            t: Arc::from(vec![1.0f32, f32::MIN_POSITIVE, -3.25]),
            nan_bits: f64::from_bits(0x7ff8_0000_0000_1234),
            arr: [1, -2, 3],
        };
        let b = to_bytes(&s).unwrap();
        let back: S = from_bytes(&b).unwrap();
        assert_eq!(back.a, s.a);
        assert_eq!(back.m, s.m);
        assert_eq!(&*back.t, &*s.t);
        assert_eq!(back.nan_bits.to_bits(), s.nan_bits.to_bits());
        assert_eq!(back.arr, s.arr);
        assert!(from_bytes::<S>(&b[..b.len() - 1]).is_err());
    }
}
